import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentPermissions, RevsetValidation, SavedRange, SearchHit, Tour } from "../contracts/http";
import type { FileDiff, GraphRow, MetricPoint, SnapshotOptions, Thread, WorkspaceSummary } from "../contracts/model";
import type { ServerEvent } from "../contracts/ws";
import type { Engine } from "../canvas/engine";
import { hex } from "../canvas/theme";
import type { Live } from "../data/live";
import type { ReviewClient } from "../data/review";
import { useEscape } from "./useEscape";
import { isOpenThread, outdatedNote, sendable } from "./reviewLogic";
import { session } from "../data/auth";
import { actorName } from "../canvas/presence";
import { markPath, markShape } from "./hotspotMarks";

const api = (path: string, init?: RequestInit) => fetch("/api/v1" + path, init).then((r) => r.json());
/** "5 min ago" from epoch seconds or an ISO-8601 time (ids.ts; older servers sent epoch strings). */
export const ago = (t: string | number) => {
  const n = Number(t);
  const secs = Number.isFinite(n) ? n : Date.parse(String(t)) / 1000;
  if (!Number.isFinite(secs)) return "";
  const d = Date.now() / 1000 - secs;
  return d < 90 ? "just now" : d < 5400 ? `${Math.round(d / 60)} min ago` : d < 172800 ? `${Math.round(d / 3600)} h ago` : `${Math.round(d / 86400)} d ago`;
};

// ─── History ────────────────────────────────────────────────────────────────

const LANE_PX = 14;

/** Fetch JSON, throwing the server's message on a non-2xx answer. */
async function get<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch("/api/v1" + path, init);
  const b = await r.json().catch(() => ({}));
  if (!r.ok) {
    const e = b.error ?? b;
    throw new Error(`${e.message ?? r.statusText}${e.details?.hint ?? e.hint ? ` · ${e.details?.hint ?? e.hint}` : ""}`);
  }
  return b as T;
}

/** What "changed since review" opens: the interdiff of a change against the commit last reviewed. */
export interface InterdiffTarget {
  change: string;
  from?: string;
  title: string;
}

/**
 * The jj graph (§7): lanes, workspaces (@), bookmarks, conflicts (×), thread counts and "changed
 * since review" (opens the interdiff). A revset field narrows it; click two rows for a range.
 */
/** Safety-net re-read of /revisions while History is open (review marks arrive as review.marked). */
const HISTORY_REFRESH_MS = 15000;

export function HistoryPanel({ engine, live, onRange, onInterdiff, onSnapshot }: { engine: Engine; live: Live; onRange: (revset: string) => void; onInterdiff: (t: InterdiffTarget) => void; onSnapshot?: (rev: string) => void }) {
  const [rows, setRows] = useState<GraphRow[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [pick, setPick] = useState<string[]>([]);
  // The revset typed in the field, and the one the list shows (applied on Enter).
  const [input, setInput] = useState("");
  const [revset, setRevset] = useState("");
  const [err, setErr] = useState<string>();
  const [note, setNote] = useState<string>();
  const load = useRef<() => void>(() => {});
  const shown = useRef("");
  useEffect(() => {
    load.current = () =>
      get<{ rows: GraphRow[]; truncated: boolean }>(`/revisions?limit=200${revset ? `&revset=${encodeURIComponent(revset)}` : ""}`)
        .then((r) => {
          // Re-render only when something changed (the panel also refreshes on a timer).
          const key = JSON.stringify(r);
          if (key === shown.current) return;
          shown.current = key;
          setRows(r.rows);
          setTruncated(r.truncated);
          setErr(undefined);
        })
        .catch((e) => setErr(String(e.message ?? e)));
    shown.current = "";
    load.current();
    // Thread counts and review marks change with reviews too, not only with ops.
    const off = live.on((e) => (e.type === "op.changed" || e.type === "review.submitted" || e.type === "review.marked" || e.type === "edit.updated" || (e.type === "thread.upserted" && e.thread.status !== "open")) && load.current());
    // Marks from other clients arrive as review.marked; a slow re-read while the panel is open and
    // the page visible (and when it comes back into view) covers a missed event.
    const timer = setInterval(() => document.visibilityState === "visible" && load.current(), HISTORY_REFRESH_MS);
    const onVisible = () => document.visibilityState === "visible" && load.current();
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      off();
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [live, revset]);
  const t = engine.theme;
  const lanes = Math.max(1, ...rows.map((r) => r.lane + 1));
  const toggle = (c: string, multi: boolean) => setPick((p) => (multi ? (p.includes(c) ? p.filter((x) => x !== c) : [...p.slice(-1), c]) : [c]));
  // Two picks make `older::newer` whichever was clicked first: the list is newest first, so the
  // row further down is the root.
  const rangeOf = (() => {
    if (pick.length === 1) return pick[0].slice(0, 8);
    if (pick.length !== 2) return "";
    const at = (c: string) => rows.findIndex((r) => r.rev.change === c);
    const [newer, older] = at(pick[0]) <= at(pick[1]) ? pick : [pick[1], pick[0]];
    return `${older.slice(0, 8)}::${newer.slice(0, 8)}`;
  })();
  const picked = pick.length === 1 ? rows.find((r) => r.rev.change === pick[0]) : undefined;
  const markReviewed = async () => {
    if (!picked) return;
    try {
      await get(`/reviews/marks/${picked.rev.change}`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ commit: picked.rev.commit }) });
      setNote(`Marked ${picked.rev.change.slice(0, 8)} reviewed at ${picked.rev.commit.slice(0, 8)}.`);
      load.current();
    } catch (e) {
      setNote(`Could not mark it reviewed: ${(e as Error).message}`);
    }
  };
  return (
    <div className="hist">
      <form
        className="revset-bar"
        onSubmit={(e) => {
          e.preventDefault();
          setRevset(input.trim());
        }}
      >
        <input className={`mono ${err ? "bad-in" : ""}`} aria-label="History revset" placeholder="revsets.log | working_copies()" value={input} onChange={(e) => setInput(e.target.value)} />
        <button type="submit">Show</button>
        {revset && (
          <button type="button" className="ghost" onClick={() => (setInput(""), setRevset(""))} title="Back to the default revset">
            Reset
          </button>
        )}
      </form>
      {err && <div className="err small pad0">{err}</div>}
      <ol>
        {rows.map((r, i) => {
          const rev = r.rev;
          const on = pick.includes(rev.change);
          const x = r.lane * LANE_PX + 8;
          return (
            <li key={rev.commit} data-change={rev.change} className={`${on ? "on" : ""} ${rev.flags.conflict ? "conflicted" : ""}`} onClick={(e) => toggle(rev.change, e.shiftKey || e.metaKey || e.ctrlKey)}>
              <svg width={lanes * LANE_PX + 6} height={44} aria-hidden>
                {/* lane line from this row to its parent's lane */}
                {r.edges.map((e, k) => (
                  <path key={k} d={`M${x},22 C${x},40 ${e.toLane * LANE_PX + 8},30 ${e.toLane * LANE_PX + 8},44`} stroke={hex(t.dim)} fill="none" strokeWidth={1.3} />
                ))}
                {i > 0 && rows[i - 1].edges.some((e) => e.toLane === r.lane) && <line x1={x} y1={0} x2={x} y2={22} stroke={hex(t.dim)} strokeWidth={1.3} />}
                {rev.flags.conflict ? (
                  // Conflicted changes get × (§6), in the conflict colour; @ keeps its marker beside it.
                  <text className="conflict-glyph" x={x} y={27} textAnchor="middle" fontSize="15" fill={hex(t.conflict)} fontWeight="700">
                    ×
                  </text>
                ) : rev.flags.workingCopy ? (
                  <text x={x} y={26} textAnchor="middle" fontSize="13" fill={hex(t.accent)} fontWeight="700">@</text>
                ) : (
                  <circle cx={x} cy={22} r={4} fill={rev.flags.immutable ? hex(t.dim) : "none"} stroke={hex(rev.flags.immutable ? t.dim : t.ink)} strokeWidth={1.5} />
                )}
              </svg>
              <div className="rev">
                <div className="top">
                  <span className="mono id">
                    <b>{rev.change.slice(0, rev.shortest.change)}</b>
                    {rev.change.slice(rev.shortest.change, 8)}
                  </span>
                  {rev.flags.workingCopy && rev.flags.conflict && <span className="mono accent">@</span>}
                  {rev.flags.conflict && <span className="chip bad">conflict</span>}
                  {rev.workspaces.map((w) => (
                    <span key={w} className="chip ws">
                      {w}@
                    </span>
                  ))}
                  {rev.bookmarks.map((b) => (
                    <span key={b} className="chip bm">
                      {b}
                    </span>
                  ))}
                  {r.review?.openThreads ? (
                    <span className="chip th" title="open review threads on this change">
                      {r.review.openThreads} thread{r.review.openThreads === 1 ? "" : "s"}
                    </span>
                  ) : null}
                  {r.review?.changedSinceReview && (
                    <button
                      className="chip warn csr"
                      title={r.review.reviewedAt ? `rewritten since you reviewed ${r.review.reviewedAt.slice(0, 8)}: show the interdiff` : "rewritten since review: show the interdiff"}
                      onClick={(e) => {
                        e.stopPropagation();
                        onInterdiff({ change: rev.change, from: r.review!.reviewedAt, title: rev.description.split("\n")[0] || rev.change.slice(0, 8) });
                      }}
                    >
                      changed since review
                    </button>
                  )}
                  {r.review?.touchedByAgent && <span className="chip ag">agent</span>}
                </div>
                <div className={`desc ${rev.description ? "" : "muted"}`}>{rev.description.split("\n")[0] || "(no description set)"}</div>
              </div>
            </li>
          );
        })}
      </ol>
      {truncated && <div className="small muted pad0">showing the first {rows.length}; narrow the revset for more</div>}
      {note && <div className="small pad0">{note}</div>}
      <footer className="hist-bar">
        <span className="muted small">{pick.length ? `${pick.length} selected · shift-click for a range` : "click a change; shift-click a second for a range"}</span>
        <div className="grow" />
        {picked && (
          <button onClick={markReviewed} title="Remember this commit as reviewed; a later rewrite shows “changed since review”">
            Mark reviewed
          </button>
        )}
        {picked && onSnapshot && (
          <button className="open-snap" onClick={() => onSnapshot(picked.rev.change)} title="Show the whole map at this change (DESIGN §7); edits there amend it, or go after it (§6)">
            Open snapshot
          </button>
        )}
        <button onClick={() => onRange(rangeOf || "trunk()..@")}>Range…</button>
        <button className="primary" disabled={pick.length !== 2} onClick={() => onRange(rangeOf)}>
          Review range
        </button>
      </footer>
    </div>
  );
}

/** The interdiff of a change against the commit last reviewed (GET /interdiff), file by file. */
export function InterdiffViewer({ target, onClose }: { target: InterdiffTarget; onClose: () => void }) {
  const [res, setRes] = useState<{ files: FileDiff[]; description?: [string, string] }>();
  const [err, setErr] = useState<string>();
  useEscape(onClose);
  useEffect(() => {
    if (!target.from) {
      setErr("The server did not say which commit was reviewed (RevisionReview.reviewedAt), so there is nothing to compare against yet.");
      return;
    }
    get<{ files: FileDiff[]; description?: [string, string] }>(`/interdiff?change=${encodeURIComponent(target.change)}&from=${encodeURIComponent(target.from)}`)
      .then(setRes)
      .catch((e) => setErr(String(e.message ?? e)));
  }, [target]);
  return (
    <div className="modal-back" onClick={onClose}>
      <div className="modal interdiff" onClick={(e) => e.stopPropagation()} role="dialog" aria-label="Interdiff">
        <header className="row">
          <h2>Changed since review</h2>
          <span className="grow" />
          <button className="x" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>
        <div className="small muted mono">
          {target.change.slice(0, 12)} · {target.from ? `from ${target.from.slice(0, 12)}` : "no reviewed commit"} · {target.title}
        </div>
        {err && (
          <div className="warn-box" role="alert">
            Interdiff unavailable: {err}
          </div>
        )}
        {!res && !err && <div className="muted small">loading…</div>}
        {res?.description && res.description[0] !== res.description[1] && (
          <div className="small">
            Description: <s className="muted">{res.description[0].split("\n")[0]}</s> → {res.description[1].split("\n")[0]}
          </div>
        )}
        {res && !res.files.length && <div className="muted small">No file changes since the review (only metadata moved).</div>}
        <div className="idiff">
          {res?.files.map((f) => (
            <section key={f.path}>
              <div className="mono small idiff-path">
                <b>{f.path}</b> <span className="muted">{f.status}</span>
              </div>
              {f.hunks.map((h, i) => (
                <pre key={i} className="idiff-hunk">
                  <span className="muted">
                    @@ −{h.old[0]},{h.old[1]} +{h.new[0]},{h.new[1]} @@
                    {"\n"}
                  </span>
                  {h.lines.map((l, k) => (
                    <span key={k} className={l.kind}>
                      {l.kind === "add" ? "+" : l.kind === "del" ? "−" : " "}
                      {l.text ?? ""}
                      {"\n"}
                    </span>
                  ))}
                </pre>
              ))}
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}

// ─── Range dialog ───────────────────────────────────────────────────────────

/** Options the dialog opens a range with (RangeOptions without the server-side defaults). */
export interface RangeOpen {
  contextHops: 0 | 1 | 2;
  diff: "cumulative" | "per-commit";
  ignoreWhitespace: boolean;
  contextDisplay: "minimized" | "hidden" | "expanded";
}

/**
 * §7 Range dialog: a revset with live validation; presets (my stack, one per agent workspace, last
 * 10, single change); a commit picker with the gap check; a fileset filter; diff, context hops,
 * context display and whitespace options; saved ranges.
 */
export function RangeDialog({ initial, onClose, onOpen }: { initial: string; onClose: () => void; onOpen: (revset: string, opts: RangeOpen, fileset?: string) => void }) {
  const [revset, setRevset] = useState(initial);
  const [fileset, setFileset] = useState("");
  const [v, setV] = useState<RevsetValidation>();
  const [hops, setHops] = useState<0 | 1 | 2>(1);
  const [diff, setDiff] = useState<RangeOpen["diff"]>("cumulative");
  const [display, setDisplay] = useState<RangeOpen["contextDisplay"]>("minimized");
  const [ws, setWs] = useState(false);
  // Saved ranges (PUT/DELETE /ranges/:name): pick one to fill the form, or save the current one.
  const [saved, setSaved] = useState<SavedRange[]>([]);
  const [name, setName] = useState("");
  // Workspaces with an agent (or any other workspace): one stack preset each.
  const [spaces, setSpaces] = useState<WorkspaceSummary[]>([]);
  // Commit picker: recent changes; checking some builds the revset.
  const [picker, setPicker] = useState<GraphRow[]>();
  const [picked, setPicked] = useState<string[]>([]);
  const opts = (): RangeOpen => ({ contextHops: hops, diff, ignoreWhitespace: ws, contextDisplay: display });
  const loadSaved = () =>
    api("/ranges")
      .then((l) => setSaved(Array.isArray(l) ? l.filter((r: SavedRange) => r?.name && r.revset) : []))
      .catch(() => setSaved([]));
  useEffect(() => {
    loadSaved();
    api("/workspaces")
      .then((l) => Array.isArray(l) && setSpaces(l))
      .catch(() => {});
  }, []);
  const openPicker = () => {
    if (picker) return setPicker(undefined);
    get<{ rows: GraphRow[] }>(`/revisions?limit=30&revset=${encodeURIComponent("ancestors(@, 30) ~ root()")}`)
      .catch(() => get<{ rows: GraphRow[] }>("/revisions?limit=30"))
      .then((r) => setPicker(r.rows))
      .catch(() => setPicker([]));
  };
  const togglePick = (change: string) => {
    const next = picked.includes(change) ? picked.filter((c) => c !== change) : [...picked, change];
    setPicked(next);
    if (next.length) setRevset(next.map((c) => c.slice(0, 12)).join(" | "));
  };
  const save = async () => {
    const n = name.trim();
    if (!n) return;
    const body = { name: n, revset, fileset: fileset.trim() || undefined, options: { ...opts(), contextLines: 3 } };
    const r = await fetch(`/api/v1/ranges/${encodeURIComponent(n)}`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify(body) }).catch(() => undefined);
    if (r?.ok) {
      setName("");
      loadSaved();
    }
  };
  const forget = async (n: string) => {
    await fetch(`/api/v1/ranges/${encodeURIComponent(n)}`, { method: "DELETE" }).catch(() => undefined);
    loadSaved();
  };
  const pickSaved = (r: SavedRange) => {
    setRevset(r.revset);
    setFileset(r.fileset ?? "");
    const o = r.options as Partial<RangeOpen> | undefined;
    if (o?.contextHops !== undefined) setHops(o.contextHops);
    if (o?.diff) setDiff(o.diff);
    if (o?.ignoreWhitespace !== undefined) setWs(o.ignoreWhitespace);
    if (o?.contextDisplay) setDisplay(o.contextDisplay);
  };
  useEffect(() => {
    const body = fileset.trim() ? { revset, fileset: fileset.trim() } : { revset };
    const t = setTimeout(() => api("/revsets:validate", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) }).then(setV).catch(() => {}), 200);
    return () => clearTimeout(t);
  }, [revset, fileset]);
  useEscape(onClose);
  const gap = v?.ok ? v.warnings.find((w) => w.kind === "gap") : undefined;
  const nonChain = v?.ok ? v.warnings.find((w) => w.kind === "multiple-roots" || w.kind === "multiple-heads") : undefined;
  const go = () => v?.ok && onOpen(revset, opts(), fileset.trim() || undefined);
  const presets: Array<[string, string, string?]> = [
    ["My stack", "trunk()..@"],
    ...spaces
      .filter((w) => !w.serving)
      .map((w): [string, string, string?] => [w.agent ? `${w.agent.name}@${w.name} stack` : `${w.name}@ stack`, w.stack?.revset ?? `trunk()..${w.name}@`, w.agent ? "agent" : undefined]),
    ["Last 10", "ancestors(@, 10) ~ root()"],
    ["Single change", picked.length === 1 ? picked[0].slice(0, 12) : "@"],
    ["Unpushed mine", "mine() & ~::trunk()"],
  ];
  return (
    <div className="modal-back" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} role="dialog" aria-label="Review a range">
        <h2>Review a range</h2>
        <label className="field">
          <span>Revset</span>
          <input className={`mono ${v && !v.ok ? "bad-in" : ""}`} aria-label="Revset" autoFocus value={revset} onChange={(e) => setRevset(e.target.value)} onKeyDown={(e) => e.key === "Enter" && go()} />
        </label>
        <div className="presets">
          {presets.map(([l, r, kind]) => (
            <button key={l} className={kind === "agent" ? "agent-preset" : ""} onClick={() => setRevset(r)}>
              {l} <span className="mono muted">{r}</span>
            </button>
          ))}
          <button className={picker ? "on" : ""} onClick={openPicker} aria-expanded={!!picker}>
            Pick commits…
          </button>
        </div>
        {picker && (
          <ul className="picker" aria-label="Commit picker">
            {!picker.length && <li className="muted small">No changes to pick from.</li>}
            {picker.map((r) => (
              <li key={r.rev.commit}>
                <label>
                  <input type="checkbox" checked={picked.includes(r.rev.change)} onChange={() => togglePick(r.rev.change)} />
                  <span className="mono id">{r.rev.change.slice(0, 8)}</span>
                  {r.rev.flags.conflict && <span className="bad"> ×</span>}
                  {r.rev.flags.immutable && <span className="muted small"> immutable</span>}
                  <span className="desc">{r.rev.description.split("\n")[0] || "(no description set)"}</span>
                </label>
              </li>
            ))}
          </ul>
        )}
        <label className="field">
          <span>Files</span>
          <input className="mono" aria-label="Fileset" placeholder='fileset, e.g. src/ | glob:"**/*.py"' value={fileset} onChange={(e) => setFileset(e.target.value)} onKeyDown={(e) => e.key === "Enter" && go()} />
        </label>
        {saved.length > 0 && (
          <div className="saved-ranges">
            <span className="muted small">Saved</span>
            {saved.map((r) => (
              <span key={r.name} className="saved">
                <button onClick={() => pickSaved(r)} title={r.revset}>
                  {r.name}
                </button>
                <button className="x-small" onClick={() => forget(r.name)} aria-label={`Delete saved range ${r.name}`} title="Delete">
                  ×
                </button>
              </span>
            ))}
          </div>
        )}
        {v && !v.ok && (
          <div className="err">
            {v.error}
            {v.hint && <span className="muted"> · {v.hint}</span>}
          </div>
        )}
        {v?.ok && (
          <div className="ok-sum">
            {v.count} changes · {v.files} files · <span className="add">+{v.added}</span> <span className="del">−{v.removed}</span>
            {v.conflicts ? <span className="err"> · {v.conflicts} conflicts</span> : null}
          </div>
        )}
        {gap && gap.kind === "gap" && (
          <div className="warn-box">
            Not contiguous: {gap.missing.length} change(s) in between are left out.{" "}
            <button onClick={() => setRevset(`(${revset})::`)}>Fill gap</button> <button onClick={() => setDiff("per-commit")}>Review per commit</button>
          </div>
        )}
        {nonChain && <div className="warn-box">Not a chain ({nonChain.kind.replace("multiple-", "several ")}): a range view needs one root and one head.</div>}
        <div className="opts">
          <label>
            Diff{" "}
            <select value={diff} onChange={(e) => setDiff(e.target.value as RangeOpen["diff"])}>
              <option value="cumulative">cumulative</option>
              <option value="per-commit">per commit</option>
            </select>
          </label>
          <label>
            Context{" "}
            <select value={hops} onChange={(e) => setHops(Number(e.target.value) as 0 | 1 | 2)}>
              <option value={0}>none</option>
              <option value={1}>1 hop</option>
              <option value={2}>2 hops</option>
            </select>
          </label>
          <label>
            Show context{" "}
            <select aria-label="Context display" value={display} onChange={(e) => setDisplay(e.target.value as RangeOpen["contextDisplay"])}>
              <option value="minimized">minimized</option>
              <option value="expanded">expanded</option>
              <option value="hidden">hidden</option>
            </select>
          </label>
          <label>
            <input type="checkbox" checked={ws} onChange={(e) => setWs(e.target.checked)} /> ignore whitespace
          </label>
        </div>
        <div className="row end">
          <input className="save-name" placeholder="Name to save as…" value={name} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.key === "Enter" && v?.ok && save()} aria-label="Saved range name" />
          <button disabled={!v?.ok || !name.trim()} onClick={save}>
            Save
          </button>
          <span className="grow" />
          <button onClick={onClose}>Cancel</button>
          <button className="primary" disabled={!v?.ok} onClick={go}>
            Open range view
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── Review ─────────────────────────────────────────────────────────────────

type ThreadFilter = "open" | "agent" | "addressed" | "all";

/** Every thread in the view, filtered; click to fly there and open it. */
export function ReviewPanel({ engine, review, onOpen }: { engine: Engine; review: ReviewClient; onOpen: (t: Thread) => void }) {
  const [, force] = useState(0);
  useEffect(() => review.onChange(() => force((n) => n + 1)), [review]);
  const [filter, setFilter] = useState<ThreadFilter>("open");
  const all = [...review.threads.values()];
  const matches = (t: Thread, f: ThreadFilter) => (f === "all" ? true : f === "agent" ? t.assignee === "agent" || t.createdBy.kind === "agent" : f === "addressed" ? t.status === "addressed" : isOpenThread(t));
  const shown = all
    .filter((t) =>
matches(t, filter))
    .sort((a, b) => Number(b.updatedAt) - Number(a.updatedAt))
    .slice(0, 300);
  const count = (f: ThreadFilter) => all.filter((t) => matches(t, f)).length;
  // The same predicate as the "open" filter (isOpenThread), minus what the agent already has.
  const forHuman = all.filter(sendable);
  const openCount = count("open");
  return (
    <div className="ins">
      {forHuman.length > 0 && (
        <div className="row">
          <button
            className="primary"
            data-testid="send-open"
            title={`Assigns these threads to the agent bound to the workspace (reviews:submit)${forHuman.length < openCount ? `; ${openCount - forHuman.length} of the ${openCount} open threads are already with the agent` : ""}`}
            onClick={() => review.submit(forHuman.map((t) => t.id), "Review submitted from the canvas")}
          >
            Send {forHuman.length}
            {forHuman.length < openCount ? ` of ${openCount}` : ""} open to agent
          </button>
        </div>
      )}
      <div className="seg small">
        {(["open", "addressed", "agent", "all"] as ThreadFilter[]).map((f) => (
          <button key={f} className={f === filter ? "on" : ""} onClick={() => setFilter(f)}>
            {f} {count(f)}
          </button>
        ))}
      </div>
      <ul className="links">
        {shown.map((t) => (
          <li key={t.id}>
            <button
              className="link thread"
              onClick={async () => {
                await engine.goTo({ path: t.anchor.path, line: t.anchor.start });
                onOpen(t);
              }}
            >
              <i className={`st ${t.status}`} />
              <span className="kind">{t.anchor.path.split("/").pop()}</span>
              <span className="name">{t.comments[t.comments.length - 1]?.body ?? t.kind}</span>
              <span className={`tstatus ${t.status}`} title={outdatedNote(t)}>
                {t.status}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

// ─── Workspaces ─────────────────────────────────────────────────────────────

/** WorkspaceSummary.stale (additive): the workspace's checkout is stale or its @ divergent. */
type Stale = { reason: "working-copy" | "divergent"; note: string; commits: string[] };
type WorkspaceCard = WorkspaceSummary & { files?: string[]; recovered?: string; stale?: Stale };

/**
 * One card per jj workspace (D12): its stack, unsnapshotted files, bound agent and overlaps. New
 * workspaces appear live (workspace.updated); Compare opens both stacks per commit on the shared files.
 */
export function WorkspacesPanel({
  engine,
  live,
  onRange,
  onCompare,
  onOpenAt,
}: {
  engine: Engine;
  live: Live;
  onRange: (revset: string) => void;
  onCompare: (a: string, b: string, paths: string[]) => void;
  /** Open a snapshot (DESIGN §7 "Open @"); rejects when the server refuses it. */
  onOpenAt?: (rev: string, options?: SnapshotOptions) => Promise<void>;
}) {
  const [cards, setCards] = useState<WorkspaceCard[]>([]);
  const [err, setErr] = useState<string>();
  useEffect(() => {
    api("/workspaces")
      .then((l) => (Array.isArray(l) ? (setCards(l), setErr(undefined)) : setErr(l?.error?.message ?? l?.message ?? "no workspace list")))
      .catch((e) => setErr(`Could not load the workspaces: ${(e as Error).message ?? e}`));
    return live.on((e) => {
      if (e.type === "workspace.updated")
        setCards((cs) => (cs.some((c) => c.name === e.workspace.name) ? cs.map((c) => (c.name === e.workspace.name ? { ...c, ...e.workspace } : c)) : [...cs, e.workspace]));
      if (e.type === "workspace.recovered") setCards((cs) => cs.map((c) => (c.name === e.workspace ? { ...c, recovered: `${e.note} (kept in ${e.divergentCommit.slice(0, 8)})` } : c)));
    });
  }, [live]);
  // "Open @" (DESIGN §7): a snapshot of that workspace's @ with its live (unsnapshotted) files, then
  // the camera goes to the first file its stack changes.
  const openAt = async (c: WorkspaceCard) => {
    if (!onOpenAt) return void (c.files?.[0] && engine.goTo({ path: c.files[0] }));
    await (c.serving ? onOpenAt("@") : onOpenAt(wsRev(c.name), { workspace: c.name })).catch(() => {});
    if (c.files?.[0]) await engine.goTo({ path: c.files[0] });
  };
  return (
    <div className="ins">
      {err && <div className="err small pad0">{err}</div>}
      {cards.map((c) => (
        <section key={c.name} className="card ws-card" data-ws={c.name}>
          <header>
            <b className="mono">{c.name}@</b>
            {c.serving && <span className="chip you">you</span>}
            {c.agent && (
              <span className={`chip ag ${c.agent.activity}`}>
                {c.agent.name}@{c.name} · {c.agent.activity}
              </span>
            )}
          </header>
          <div className="small muted mono">{c.root}</div>
          <div className="small">
            {c.stack.changes} changes · {c.stack.files} files{c.stack.conflicts ? ` · ${c.stack.conflicts} conflicts` : ""}
            {c.unsnapshottedFiles ? <span className="add"> · {c.unsnapshottedFiles} edited, not snapshotted</span> : ""} · {ago(c.lastOp.time)}
          </div>
          {c.recovered && <div className="warn-box small">Recovered: {c.recovered}</div>}
          {c.stale && (
            <div className="warn-box small stale" role="alert">
              <b>{c.stale.reason === "working-copy" ? "Stale working copy" : "Divergent"}</b>
              {c.stale.reason === "working-copy" ? (
                <>
                  {" "}
                  — run <code>jj workspace update-stale</code> in {c.name}
                </>
              ) : null}
              {c.stale.note ? <> · {c.stale.note}</> : null}
              {c.stale.commits?.length ? <span className="mono muted"> ({c.stale.commits.map((x) => x.slice(0, 8)).join(", ")})</span> : null}
            </div>
          )}
          {c.overlaps.map((o) => (
            <div key={o.workspace} className="warn-box small">
              Overlaps {o.workspace}@ on {o.paths.length} file{o.paths.length === 1 ? "" : "s"}:{" "}
              {o.paths.map((p) => (
                <button key={p} className="linkish" onClick={() => engine.goTo({ path: p })}>
                  {p.split("/").pop()}
                </button>
              ))}{" "}
              <button className="compare" onClick={() => onCompare(c.name, o.workspace, o.paths)} title={`Both stacks per commit, filtered to the ${o.paths.length} shared file(s)`}>
                Compare
              </button>
            </div>
          ))}
          <div className="row">
            <button className="open-at" onClick={() => void openAt(c)} title={`A snapshot of ${c.name}@ with its unsnapshotted files`}>
              Open @
            </button>
            <button onClick={() => onRange(c.stack.revset)}>Review stack</button>
          </div>
        </section>
      ))}
    </div>
  );
}

/** The revset for a workspace's @ (`name@`, quoted when the name needs it), as the server spells it. */
export function wsRev(name: string) {
  return /^[A-Za-z0-9_.]+$/.test(name) ? `${name}@` : `${JSON.stringify(name)}@`;
}

/**
 * The top bar's revision chip (DESIGN §7): open a snapshot at another revision (anything that resolves
 * to one commit) or of another workspace's @ with its live files. Edits made there go where §6 says
 * for that commit: amend it by default, or insert after it, branch from it, keep descendants; an
 * immutable commit only branches.
 */
export function RevisionPicker({ current, onOpen, onClose }: { current: string; onOpen: (rev: string, options?: SnapshotOptions) => Promise<void>; onClose: () => void }) {
  const [rev, setRev] = useState(current);
  const [liveFiles, setLiveFiles] = useState(true);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string>();
  const [cards, setCards] = useState<WorkspaceSummary[]>([]);
  useEscape(onClose);
  useEffect(() => {
    api("/workspaces")
      .then((l) => Array.isArray(l) && setCards(l))
      .catch(() => {});
  }, []);
  const open = async (r: string, options?: SnapshotOptions) => {
    const text = r.trim();
    if (!text) return;
    setBusy(true);
    setErr(undefined);
    try {
      await onOpen(text, liveFiles ? options : { ...options, liveWorkingCopy: false });
      onClose();
    } catch (e) {
      setErr((e as Error).message ?? String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="revpick" role="dialog" aria-label="Open a revision">
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          void open(rev);
        }}
      >
        <input className={`mono ${err ? "bad-in" : ""}`} aria-label="Revision" autoFocus value={rev} placeholder="@-, a change id, main, trunk()" onChange={(e) => (setRev(e.target.value), setErr(undefined))} />
        <button type="submit" className="primary" disabled={busy}>
          Open
        </button>
      </form>
      {err && <div className="err small">{err}</div>}
      <div className="small muted">Revisions</div>
      <div className="row wrap">
        {["@", "@-", "trunk()"].map((r) => (
          <button key={r} className="chip mono" disabled={busy} onClick={() => void open(r)}>
            {r}
          </button>
        ))}
      </div>
      {cards.length > 1 && (
        <>
          <div className="small muted">Workspaces (their @ with unsnapshotted files)</div>
          <div className="row wrap">
            {cards.map((c) => (
              <button key={c.name} className="chip mono ws" data-ws={c.name} disabled={busy} onClick={() => void (c.serving ? open("@") : open(wsRev(c.name), { workspace: c.name }))}>
                {c.name}@{c.serving ? " (you)" : ""}
              </button>
            ))}
          </div>
        </>
      )}
      <label className="row small">
        <input type="checkbox" checked={liveFiles} onChange={(e) => setLiveFiles(e.target.checked)} /> Show unsnapshotted files (live working copy)
      </label>
    </div>
  );
}

// ─── Hotspots ───────────────────────────────────────────────────────────────

const QUADS = [
  ["Active leaves", 0.25, 0.25],
  ["Review first", 0.75, 0.25],
  ["Quiet periphery", 0.25, 0.75],
  ["Foundations", 0.75, 0.75],
] as const;

/** Scatter of clusters (x connectedness, y change frequency) with a brush that filters the canvas. */
export function HotspotsPanel({ engine }: { engine: Engine }) {
  const [points, setPoints] = useState<MetricPoint[]>([]);
  const [brush, setBrush] = useState<[number, number, number, number]>();
  // Range views (§4.7): rank by how many range commits touch each cluster instead of by risk.
  const [rank, setRank] = useState<"risk" | "range">(engine.isRange ? "range" : "risk");
  const drag = useRef<[number, number]>(undefined);
  const svg = useRef<SVGSVGElement>(null);
  useEffect(() => {
    // A range view reuses its tip's map; when it has no metrics of its own, use the shared placement's.
    const v = engine.client.view;
    get<{ points: MetricPoint[] }>(`/views/${v.id}/metrics?level=cluster${engine.isRange ? "&rank=inRange" : ""}`)
      .catch(() => (v.sharedPlacement ? get<{ points: MetricPoint[] }>(`/views/${v.sharedPlacement}/metrics?level=cluster`) : Promise.reject()))
      .then((r) => setPoints(r.points))
      .catch(() => setPoints([]));
    return () => engine.setBrush(undefined);
  }, [engine]);
  const touches = useMemo(() => {
    if (!engine.isRange) return new Map<string, { touches: number; hunks: number }>();
    // The server ranks range views itself (metrics?rank=inRange → heat.inRange); else count loaded hunks.
    const served = points.filter((p) => p.heat.inRange);
    if (served.length) return new Map(served.map((p) => [p.key as string, { touches: p.heat.inRange!.commits, hunks: 0 }]));
    return inRangeTouches(engine);
  }, [engine, points]);
  const t = engine.theme;
  const S = 260;
  const inBrush = (p: MetricPoint) => !brush || (p.connPct >= brush[0] && p.connPct <= brush[2] && 1 - p.heatPct >= brush[1] && 1 - p.heatPct <= brush[3]);
  const ranked = useMemo(() => {
    const list = points.filter(inBrush);
    if (rank === "range") {
      // Clusters the range touches, most touched first; ones without a metrics point still list.
      const byKey = new Map(list.map((p) => [p.key as string, p]));
      return [...touches.entries()]
        .filter(([k]) => !brush || byKey.has(k))
        .sort((a, b) => b[1].touches - a[1].touches || b[1].hunks - a[1].hunks)
        .map(([k]) => byKey.get(k) ?? clusterPoint(engine, k))
        .filter((p): p is MetricPoint => !!p);
    }
    return list.sort((a, b) => b.risk - a.risk);
  }, [points, brush, rank, touches]);
  useEffect(() => {
    engine.setBrush(brush ? new Set(points.filter(inBrush).map((p) => p.key as string)) : undefined);
  }, [brush, points]);
  const at = (e: React.PointerEvent): [number, number] => {
    const r = svg.current!.getBoundingClientRect();
    return [Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)), Math.max(0, Math.min(1, (e.clientY - r.top) / r.height))];
  };
  const shape = (p: MetricPoint, x: number, y: number) => {
    const c = hex(t.heat[p.heat.grade]);
    const o = inBrush(p) ? 0.9 : 0.15;
    const m = markPath(markShape(p.lane), x, y);
    if (!m) return <circle cx={x} cy={y} r={2.6} fill={c} opacity={o} data-lane={p.lane} />;
    return m.stroke ? <path d={m.d} fill="none" stroke={c} strokeWidth={1.6} strokeLinecap="round" opacity={o} data-lane={p.lane} /> : <path d={m.d} fill={c} opacity={o} data-lane={p.lane} />;
  };
  return (
    <div className="ins">
      <svg
        ref={svg}
        className="scatter"
        viewBox={`0 0 ${S} ${S}`}
        onPointerDown={(e) => {
          (e.target as Element).setPointerCapture?.(e.pointerId);
          drag.current = at(e);
          setBrush(undefined);
        }}
        onPointerMove={(e) => {
          if (!drag.current) return;
          const [x, y] = at(e);
          const [x0, y0] = drag.current;
          setBrush([Math.min(x0, x), Math.min(y0, y), Math.max(x0, x), Math.max(y0, y)]);
        }}
        onPointerUp={() => (drag.current = undefined)}
      >
        <rect x={S / 2} y={0} width={S / 2} height={S / 2} fill={hex(t.accent)} opacity={0.08} />
        <line x1={S / 2} y1={0} x2={S / 2} y2={S} stroke={hex(t.dim)} strokeDasharray="3 3" />
        <line x1={0} y1={S / 2} x2={S} y2={S / 2} stroke={hex(t.dim)} strokeDasharray="3 3" />
        {QUADS.map(([l, x, y]) => (
          <text key={l} x={x * S} y={y * S} textAnchor="middle" fontSize="10" fill={hex(t.dim)}>
            {l}
          </text>
        ))}
        {points.map((p) => (
          <g key={p.key as string}>{shape(p, p.connPct * S, (1 - p.heatPct) * S)}</g>
        ))}
        {brush && <rect x={brush[0] * S} y={brush[1] * S} width={(brush[2] - brush[0]) * S} height={(brush[3] - brush[1]) * S} fill={hex(t.accent)} fillOpacity={0.12} stroke={hex(t.accent)} strokeDasharray="4 3" />}
      </svg>
      <div className="axes small muted">
        <span>↑ changes often</span>
        <span>many dependents →</span>
      </div>
      <div className="row">
        <span className="small muted">{brush ? `${ranked.length} clusters brushed; the canvas fades the rest` : "drag on the chart to filter the canvas"}</span>
        {brush && <button onClick={() => setBrush(undefined)}>Clear</button>}
      </div>
      <div className="row">
        <h3 className="grow">{rank === "range" ? "Ranked by in-range touches" : "Ranked by risk"}</h3>
        {engine.isRange && (
          <div className="seg small" role="group" aria-label="Rank by">
            <button className={rank === "risk" ? "on" : ""} onClick={() => setRank("risk")}>
              risk
            </button>
            <button className={rank === "range" ? "on" : ""} onClick={() => setRank("range")}>
              in-range touches
            </button>
          </div>
        )}
      </div>
      <ul className="links ranked">
        {ranked.slice(0, 60).map((p) => (
          <li key={p.key as string}>
            <button
              className="link"
              onClick={() => {
                const c = engine.client.boot.clusters.find((c) => c.id === p.key);
                if (c) (engine.select({ type: "cluster", c }), engine.focusTarget({ type: "cluster", c }));
              }}
            >
              <i style={{ background: hex(t.heat[p.heat.grade]) }} />
              <span className="kind">{rank === "range" ? `${touches.get(p.key as string)?.touches ?? 0}×` : `${(p.risk * 100).toFixed(0)}%`}</span>
              <span className="name">{p.label}</span>
              <span className="path">{p.lane}</span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * In-range touches per cluster of a range view: the sum over its hunk windows of how many range
 * commits touch each (heat.inRange), counting a hunk once when the server leaves that out.
 */
function inRangeTouches(engine: Engine) {
  const m = new Map<string, { touches: number; hunks: number }>();
  for (const w of engine.client.windows.values()) {
    if (w.kind !== "hunk") continue;
    const e = m.get(w.cluster) ?? { touches: 0, hunks: 0 };
    e.touches += w.heat.inRange?.commits ?? 1;
    e.hunks++;
    m.set(w.cluster, e);
  }
  return m;
}

/** A ranking row for a cluster without a metrics point (bootstrap data only). */
function clusterPoint(engine: Engine, id: string): MetricPoint | undefined {
  const c = engine.client.boot.clusters.find((c) => c.id === id);
  if (!c) return undefined;
  return {
    key: c.id as MetricPoint["key"],
    label: c.label,
    lane: c.lane,
    heat: { grade: c.heat, rate: 0, ci: [0, 0], settled: true, touches: 0 },
    conn: { grade: c.conn, dependents2: 0, fanIn: 0, fanOut: 0, tests: 0, reach: 0 },
    heatPct: 0,
    connPct: 0,
    risk: 0,
  };
}

// ─── Agent ──────────────────────────────────────────────────────────────────

interface FeedItem {
  at: number;
  text: string;
  windows?: string[];
  thread?: Thread;
}

/** What agents are doing: attention, threads they touched, edits they made. Follow mode flies along. */
export function AgentPanel({ engine, live, onOpen, follow, setFollow, onTour }: { engine: Engine; live: Live; onOpen: (t: Thread) => void; follow: boolean; setFollow: (f: boolean) => void; onTour: (t: Tour) => void }) {
  const [feed, setFeed] = useState<FeedItem[]>([]);
  const [tours, setTours] = useState<Tour[]>([]);
  const [perms, setPerms] = useState<AgentPermissions>();
  useEffect(() => {
    api("/tours").then((t) => Array.isArray(t) && setTours(t)).catch(() => {});
    // Per-session permissions (§10): this tab's session; older servers ignore it and answer globally.
    api(`/agent/permissions?session=${encodeURIComponent(session())}`).then((p) => p && "edit" in p && setPerms(p)).catch(() => {});
    return live.on((e) => {
      if (e.type === "tour.created") setTours((ts) => [e.tour, ...ts.filter((t) => t.id !== e.tour.id)]);
      // Toggles are per UI session (§10): another tab's change is not this tab's.
      if (e.type === "agent.permissions" && !e.permissions.effective && (!e.permissions.session || e.permissions.session === session())) setPerms(e.permissions);
    });
  }, [live]);
  // The camera toggle shows what is stored for this session (and App follows it).
  useEffect(() => {
    if (perms && perms.moveCamera !== follow) setFollow(perms.moveCamera);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [perms?.moveCamera]);
  // Optimistic: the toggle flips at once; the server's answer (and agent.permissions) confirms it.
  const setPerm = (k: keyof AgentPermissions, v: boolean) => {
    setPerms((p) => (p ? { ...p, [k]: v } : p));
    return api(`/agent/permissions?session=${encodeURIComponent(session())}`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ [k]: v, session: session() }) }).then(setPerms);
  };
  const followRef = useRef(follow);
  followRef.current = follow;
  useEffect(
    () =>
      live.on((e: ServerEvent) => {
        let item: FeedItem | undefined;
        if (e.type === "agent.attention") {
          item = { at: Date.now(), text: `${e.agent}${e.workspace ? ` (${e.workspace}@)` : ""} · ${e.tool}${e.note ? ` — ${e.note}` : ""}`, windows: e.windows };
          if (followRef.current && e.windows[0]) engine.goTo({ window: e.windows[0] });
        } else if (e.type === "thread.upserted") {
          const last = e.thread.comments[e.thread.comments.length - 1];
          if (last?.author.kind === "agent" || e.thread.assignee === "agent")
            item = { at: Date.now(), text: `${last?.author.kind === "agent" ? `${actorName(last.author)} replied` : "thread"} · ${e.thread.status}: ${last?.body.split("\n")[0] ?? ""}`, thread: e.thread };
        } else if (e.type === "edit.updated" && e.edit.actor.kind === "agent") {
          item = { at: Date.now(), text: `${actorName(e.edit.actor)} ${e.edit.status} an edit (${e.edit.rewritten.join(", ")})` };
        }
        if (item) setFeed((f) => [item!, ...f].slice(0, 100));
      }),
    [live, engine],
  );
  return (
    <div className="ins">
      <fieldset className="perms">
        <legend className="small muted">This session</legend>
        <label className="row small">
          <input type="checkbox" checked={perms?.edit ?? true} disabled={!perms} onChange={(e) => setPerm("edit", e.target.checked)} /> Agent may edit code
        </label>
        <label className="row small">
          <input
            type="checkbox"
            checked={perms?.moveCamera ?? follow}
            onChange={(e) => {
              setFollow(e.target.checked);
              if (perms) setPerm("moveCamera", e.target.checked);
            }}
          />{" "}
          Agent may move my camera (follow what it looks at)
        </label>
      </fieldset>
      {tours.length > 0 && (
        <>
          <h3>Tours · {tours.length}</h3>
          <ul className="links">
            {tours.map((t) => (
              <li key={t.id}>
                <button className="link" onClick={() => onTour(t)}>
                  <i style={{ background: "#ff6ec7" }} />
                  <span className="kind">{t.steps.length} stops</span>
                  <span className="name">{t.title}</span>
                  <span className="path">{t.by.kind === "agent" ? t.by.name : "you"}</span>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
      <h3>Activity</h3>
      {!feed.length && <p className="muted small">No agent activity yet. Assign a thread to the agent ("Send to agent" or "Ask agent") to start the loop.</p>}
      <ol className="feed">
        {feed.map((f, i) => (
          <li key={i}>
            <span className="muted small">{new Date(f.at).toLocaleTimeString()}</span>{" "}
            {f.thread ? (
              <button className="linkish" onClick={() => onOpen(f.thread!)}>
                {f.text}
              </button>
            ) : f.windows?.length ? (
              <button className="linkish" onClick={() => engine.goTo({ window: f.windows![0] })}>
                {f.text}
              </button>
            ) : (
              f.text
            )}
          </li>
        ))}
      </ol>
    </div>
  );
}

// ─── Search ─────────────────────────────────────────────────────────────────

/** ⌘K search over symbols, files, hubs and threads; Enter flies to the hit. */
export function SearchBox({ engine, onThread }: { engine: Engine; onThread: (id: string) => void }) {
  const [q, setQ] = useState("");
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [idx, setIdx] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        input.current?.focus();
        input.current?.select();
      }
    };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, []);
  useEffect(() => {
    if (q.trim().length < 2) return setHits([]);
    const t = setTimeout(() => api(`/search?q=${encodeURIComponent(q)}&limit=20`).then((h) => (setHits(h), setIdx(0))), 120);
    return () => clearTimeout(t);
  }, [q]);
  const go = async (h: SearchHit) => {
    setHits([]);
    input.current?.blur();
    const [path, line] = (h.detail ?? "").split(":");
    if (h.target.thread) {
      await engine.goTo({ path: h.target.site?.path, line: h.target.site?.line });
      onThread(h.target.thread);
    } else if (h.target.hub) {
      // Hubs placed on the canvas fly to their diamond; the rest to their first site.
      const hub = engine.client.boot.hubs.find((x) => x.id === h.target.hub);
      if (hub?.rect) engine.focusTarget({ type: "hub", h: hub });
      else if (h.target.site) await engine.goTo({ path: h.target.site.path, line: h.target.site.line });
    } else await engine.goTo({ window: engine.client.windows.has(h.target.window ?? "") ? h.target.window : undefined, path, line: Number(line) || undefined });
  };
  return (
    <div className="search-wrap">
      <input
        ref={input}
        className="search"
        placeholder="Search symbols, files, topics, threads… ⌘K"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") setIdx((i) => Math.min(hits.length - 1, i + 1));
          else if (e.key === "ArrowUp") setIdx((i) => Math.max(0, i - 1));
          else if (e.key === "Enter" && hits[idx]) go(hits[idx]);
          else if (e.key === "Escape") (setHits([]), input.current?.blur());
          else return;
          e.preventDefault();
        }}
      />
      {hits.length > 0 && (
        <ul className="hits">
          {hits.map((h, i) => (
            <li key={i} className={i === idx ? "on" : ""} onMouseDown={(e) => (e.preventDefault(), go(h))}>
              <span className="kind">{h.kind}</span>
              <span className="name">{h.label}</span>
              <span className="path mono">{h.detail}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ─── Tour player ────────────────────────────────────────────────────────────

/** Plays an agent's tour: one stop at a time, the camera flies there and the note explains it. */
export function TourPlayer({ engine, tour, onClose }: { engine: Engine; tour: Tour; onClose: () => void }) {
  const [i, setI] = useState(0);
  const step = tour.steps[i];
  useEffect(() => {
    if (step) engine.goTo({ path: step.target.path, line: step.lines?.[0] ?? step.target.line });
  }, [engine, step]);
  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement).closest?.("input,textarea")) return;
      if (e.key === "Escape") onClose();
      else if (e.key === "]" || e.key === "PageDown") setI((x) => Math.min(tour.steps.length - 1, x + 1));
      else if (e.key === "[" || e.key === "PageUp") setI((x) => Math.max(0, x - 1));
    };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [tour, onClose]);
  if (!step) return null;
  return (
    <div className="tour" role="dialog" aria-label={`Tour: ${tour.title}`}>
      <div className="tour-head">
        <b>{tour.title}</b>
        <span className="muted small">
          {" "}
          · stop {i + 1} of {tour.steps.length} · by {tour.by.kind === "agent" ? tour.by.name : "you"}
        </span>
        <div className="grow" />
        <button className="x" onClick={onClose} aria-label="Close tour">
          ×
        </button>
      </div>
      <div className="mono small muted">
        {step.target.path}:{step.lines ? `${step.lines[0]}–${step.lines[1]}` : step.target.line}
      </div>
      <p>{step.note}</p>
      <div className="row">
        <button disabled={i === 0} onClick={() => setI(i - 1)}>
          ← Back
        </button>
        <button className="primary" disabled={i === tour.steps.length - 1} onClick={() => setI(i + 1)}>
          Next →
        </button>
      </div>
    </div>
  );
}
