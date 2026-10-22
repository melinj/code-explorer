import { useEffect, useRef, useState } from "react";
import type { ViewBootstrap } from "../contracts/http";
import type { ChurnSummary, Lens, SnapshotOptions, ViewSpec } from "../contracts/model";
import { bench, SCENARIOS } from "../canvas/bench";
import { GROUP_LABEL } from "../canvas/describe";
import { LINK_GROUPS, edgeColor, type LinkGroup } from "../canvas/edges";
import { Engine, type EngineStats } from "../canvas/engine";
import type { Target } from "../canvas/overlay";
import { DARK, LIGHT, applyCssVars, hex } from "../canvas/theme";
import { DataClient } from "../data/client";
import { Inspector } from "./Inspector";
import { Composer, ConflictPopover, Editor, ThreadPopover, UndoBar } from "./Review";
import { ReviewClient } from "../data/review";
import type { CanvasAction } from "../canvas/engine";
import { Live } from "../data/live";
import { invalidationAction } from "../data/invalidation";
import { AgentPanel, HistoryPanel, HotspotsPanel, InterdiffViewer, RangeDialog, ReviewPanel, RevisionPicker, SearchBox, TourPlayer, WorkspacesPanel, type InterdiffTarget, type RangeOpen } from "./Panels";
import { session } from "../data/auth";
import { actorName, presenceName } from "../canvas/presence";
import { drawnStubs } from "../canvas/chunks";
import type { Tour } from "../contracts/http";

type Tab = "inspector" | "history" | "review" | "workspaces" | "hotspots" | "agent";
const TABS: Array<[Tab, string]> = [
  ["inspector", "Inspector"],
  ["history", "History"],
  ["review", "Review"],
  ["workspaces", "Workspaces"],
  ["hotspots", "Hotspots"],
  ["agent", "Agent"],
];

declare global {
  interface Window {
    __cv?: { engine: Engine; bench: (name: string) => ReturnType<typeof bench>; scenarios: string[]; openInterdiff?: (t: InterdiffTarget) => void; live?: Live; stubs?: () => string[] };
  }
}

const TIER_NAMES = { L0: "Galaxy", L1: "Map", L2: "Outline", L3: "Code" } as const;

/** An agent asked the human to look somewhere (focus.request) or opened a view (view.requested) while not followed. */
interface Suggestion {
  id: number;
  by: string;
  text: string;
  note?: string;
  go: () => void;
}

/** "graded over 340 commits · 93% settled" (§4.7 stopping rule result). */
function churnText(c: ChurnSummary) {
  return `graded over ${c.scannedCommits.toLocaleString()} commits · ${Math.round(c.settledMass * 100)}% settled`;
}

function specText(spec: ViewSpec) {
  return spec.mode === "range" ? `range ${spec.revset}` : `snapshot ${spec.rev ?? "@"}`;
}
/** A representative kind per group, for the legend swatch. */
const GROUP_KIND = { calls: "call", inherit: "inherit", events: "publish", http: "http-request", config: "read" } as const;
const LENSES: Array<[Lens, string]> = [
  ["none", "None"],
  ["heat", "Heat"],
  ["connectedness", "Depends"],
  ["hotspots", "Hotspots"],
  ["cochange", "Co-change"],
];

function useSystemDark() {
  const [dark, setDark] = useState(() => matchMedia("(prefers-color-scheme: dark)").matches);
  useEffect(() => {
    const m = matchMedia("(prefers-color-scheme: dark)");
    const on = () => setDark(m.matches);
    m.addEventListener("change", on);
    return () => m.removeEventListener("change", on);
  }, []);
  return dark;
}

export function App() {
  const host = useRef<HTMLDivElement>(null);
  const engine = useRef<Engine>(undefined);
  const [boot, setBoot] = useState<ViewBootstrap>();
  const [stats, setStats] = useState<EngineStats>();
  const [error, setError] = useState<string>();
  // The selection object is reused while its links load, so carry a version to re-render.
  const [sel, setSel] = useState<{ t?: Target; v: number }>({ v: 0 });
  const [hidden, setHidden] = useState<ReadonlySet<LinkGroup>>(new Set());
  const [lens, setLens] = useState<Lens>("none");
  const [hiddenLanes, setHiddenLanes] = useState<ReadonlySet<string>>(new Set());
  // Narrow screens: the side panel is a drawer over the canvas.
  const [drawer, setDrawer] = useState(false);
  const [action, setAction] = useState<CanvasAction>();
  const review = useRef(new ReviewClient()).current;
  const live = useRef(new Live()).current;
  const [tab, setTab] = useState<Tab>("inspector");
  const [liveStatus, setLiveStatus] = useState<Live["status"]>("connecting");
  const [agent, setAgent] = useState<{ name: string; tool: string; at: number }>();
  const [suggestions, setSuggestions] = useState<Suggestion[]>([]);
  const [interdiff, setInterdiff] = useState<InterdiffTarget>();
  const [repoChurn, setRepoChurn] = useState<ChurnSummary>();
  const [range, setRange] = useState<string>();
  const [rangeView, setRangeView] = useState<string>();
  const [followAgent, setFollowAgent] = useState(false);
  // The snapshot on screen (or the one "Snapshot" returns to): its revision and commit, for the top-bar chip.
  const [snap, setSnap] = useState<{ rev: string; head?: string; workspace?: string }>({ rev: "@" });
  const [revPicker, setRevPicker] = useState(false);
  const [tour, setTour] = useState<Tour>();
  // Short-lived notices (a refused move or pin).
  const [toast, setToast] = useState<string>();
  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(undefined), 4000);
    return () => clearTimeout(t);
  }, [toast]);
  // index.progress while the server (re)indexes: shown in the status bar until layout finishes.
  const [indexing, setIndexing] = useState<{ job: string; done: number; total: number }>();
  const followRef = useRef(followAgent);
  followRef.current = followAgent;
  const stateRef = useRef({ tab, lens });
  stateRef.current = { tab, lens };
  const systemDark = useSystemDark();
  const [themePref, setThemePref] = useState<"system" | "dark" | "light">("system");
  const dark = themePref === "system" ? systemDark : themePref === "dark";

  useEffect(() => {
    let cancelled = false;
    let e: Engine | undefined;
    const cleanup: Array<() => void> = [];
    (async () => {
      const client = new DataClient();
      // The canvas glyph atlases rasterize once, so the web fonts must be in before the engine starts
      // (bounded: offline, the atlases fall back to system fonts rather than delaying the map).
      const [b] = await Promise.all([client.open(), fontsReady()]);
      if (cancelled) return;
      e = new Engine(host.current!, client, review);
      engine.current = e;
      e.onAction = setAction;
      e.onLayoutError = setToast;
      const noteView = () => {
        const spec = e!.client.view.spec as ViewSpec;
        if (spec.mode === "snapshot") setSnap({ rev: spec.rev ?? "@", head: e!.client.view.resolved.head, workspace: spec.options?.workspace });
      };
      noteView();
      e.onViewChange = (id) => (live.subscribe(id), noteView());
      e.onViewRecovered = setToast;
      // Per-session permissions (§10): this tab's stored toggles, not another tab's.
      fetch(`/api/v1/agent/permissions?session=${encodeURIComponent(session())}`)
        .then((r) => (r.ok ? r.json() : undefined))
        .then((p) => typeof p?.moveCamera === "boolean" && setFollowAgent(p.moveCamera))
        .catch(() => {});
      review.load().catch((err) => console.error("threads", err));
      review.loadConflicts();
      fetch("/api/v1/repo")
        .then((r) => (r.ok ? r.json() : undefined))
        .then((r) => r?.index?.churn && setRepoChurn(r.index.churn))
        .catch(() => {});
      let sid = 0;
      const suggest = (x: Omit<Suggestion, "id">) => setSuggestions((l) => [{ ...x, id: ++sid }, ...l].slice(0, 3));
      // Back after the socket dropped (the server may have restarted): is the view still there?
      let wasOffline = false;
      live.onStatus = (st) => {
        setLiveStatus(st);
        if (st === "offline") wasOffline = true;
        else if (st === "live" && wasOffline) {
          wasOffline = false;
          void e!.checkView();
        }
      };
      // Events were missed beyond the server's retention: reload what they would have updated.
      live.onResync = () => {
        review.load().catch(() => {});
        review.loadConflicts();
        e!.reloadLayout();
      };
      live.on((ev) => {
        switch (ev.type) {
          case "thread.upserted":
            review.upsert(ev.thread);
            break;
          case "view.invalidated": {
            setIndexing(undefined);
            // What the event asks of this tab (data/invalidation.ts).
            const act = invalidationAction(ev, { id: e!.client.view.id, isRange: e!.isRange });
            if (act.kind === "reload-layout") e!.reloadLayout();
            else if (act.kind === "windows") e!.refreshWindows(act.windows);
            else if (act.kind === "tiles") e!.refreshTiles(act.tiles);
            else if (act.kind === "bootstrap") e!.reloadBootstrap();
            break;
          }
          case "view.requested": {
            // The agent opened a view: switch while following it, otherwise offer it.
            if (ev.viewId === e!.client.view.id) break;
            const open = () => e!.openView(ev.spec).then(() => setRangeView(ev.spec.mode === "range" ? ev.spec.revset : undefined)).catch((x) => setToast(`Could not open it: ${(x as Error).message}`));
            if (followRef.current) open();
            else suggest({ by: ev.by.kind === "agent" ? actorName(ev.by) : "someone", text: `opened ${specText(ev.spec)}`, go: open });
            break;
          }
          case "agent.permissions":
            // Toggles are per UI session (§10): another tab's change is not this tab's.
            if (ev.permissions.effective || (ev.permissions.session && ev.permissions.session !== session())) break;
            setFollowAgent(ev.permissions.moveCamera);
            break;
          case "focus.request": {
            // The agent asks the human to look somewhere: fly there while following, else suggest it.
            const go = () => e!.goTo({ path: ev.target.path, line: ev.target.line });
            if (followRef.current) go();
            else suggest({ by: ev.by.kind === "agent" ? actorName(ev.by) : "someone", text: `suggests ${ev.target.path.split("/").pop()}:${ev.lines ? `${ev.lines[0]}–${ev.lines[1]}` : ev.target.line}`, note: ev.note, go });
            break;
          }
          case "review.submitted":
            // Threads were assigned to the agent: reload them (markers, Review tab, history counts).
            review.load().catch(() => {});
            setToast(`${ev.threads.length} thread${ev.threads.length === 1 ? "" : "s"} sent to the agent${ev.note ? ` · ${ev.note}` : ""}`);
            break;
          case "workspace.recovered":
            // An edit left a workspace's @ divergent; its edits sit in the divergent sibling (§6 step 3).
            setToast(`${ev.workspace}@ recovered: ${ev.note} (kept in ${ev.divergentCommit.slice(0, 8)})`);
            review.loadConflicts();
            e!.reloadLayout();
            break;
          case "edit.updated":
            e!.refreshAfterEdit(ev.edit);
            break;
          case "conflicts.changed":
            for (const c of ev.resolved) e!.clearConflicts(c.path);
            review.loadConflicts();
            break;
          case "index.progress":
            // layout ends an index pass; reanchor (threads following an op) is a pass of its own.
            setIndexing((ev.job === "layout" || ev.job === "reanchor") && ev.done >= ev.total ? undefined : { job: ev.job, done: ev.done, total: ev.total });
            break;
          case "agent.attention":
            e!.attention({ agent: ev.agent, workspace: ev.workspace, tool: ev.tool, windows: ev.windows, note: ev.note, at: Date.now() });
            setAgent({ name: presenceName(ev), tool: ev.tool, at: Date.now() });
            break;
        }
      });
      live.start();
      live.subscribe(client.view.id);
      // What the human is looking at, for MCP get_view_state (4 Hz, only when it changes).
      let last = "";
      const timer = setInterval(() => {
        const c = e!.cam;
        const v = c.view();
        const sel = e!.selected;
        const state = {
          viewId: e!.client.view.id,
          viewport: { rect: { x: Math.round(v.x), y: Math.round(v.y), w: Math.round(v.w), h: Math.round(v.h) }, zoom: +c.zoom.toFixed(4), tier: c.tier },
          focus: sel?.type === "window" ? sel.w.id : undefined,
          selectedFile: sel?.type === "window" && sel.w.fragment ? sel.w.path : undefined,
          lens: stateRef.current.lens,
          panel: stateRef.current.tab === "inspector" ? undefined : stateRef.current.tab,
          followAgent: followRef.current,
        };
        const json = JSON.stringify(state);
        if (json !== last) {
          last = json;
          live.send({ type: "view.state", state } as never);
        }
      }, 250);
      cleanup.push(() => clearInterval(timer));
      await e.start(setStats, (t) => setSel((s) => ({ t, v: s.v + 1 })));
      // DESIGN §4.3: lanes the server marks collapsed (docs, by default) start closed: their pill
      // is off and their windows are not drawn until it is clicked.
      e.onHiddenLanes = (lanes) => setHiddenLanes(lanes);
      const collapsed = new Set(b.laneDefs.filter((l) => l.collapsed && !l.hidden).map((l) => l.id));
      if (collapsed.size) {
        e.setHiddenLanes(collapsed);
        setHiddenLanes(collapsed);
      }
      setBoot(b);
      window.__cv = { engine: e, bench: (n) => bench(e!, n), scenarios: Object.keys(SCENARIOS), openInterdiff: setInterdiff, live, stubs: () => [...drawnStubs] };
    })().catch((err) => setError(String(err)));
    return () => {
      cancelled = true;
      cleanup.forEach((f) => f());
      live.stop();
      e?.destroy();
    };
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    applyCssVars(dark ? DARK : LIGHT);
    if (boot) engine.current?.setTheme(dark ? DARK : LIGHT);
  }, [dark, boot]);

  const openThread = async (t: import("../contracts/model").Thread) => {
    const w = await engine.current?.goTo({ path: t.anchor.path, line: t.anchor.start });
    if (w) setAction({ type: "thread", thread: t, window: w });
  };
  const toggleGroup = (g: LinkGroup) => {
    const next = new Set(hidden);
    if (next.has(g)) next.delete(g);
    else next.add(g);
    setHidden(next);
    engine.current?.setHiddenGroups(next);
  };
  const [nonChain, setNonChain] = useState<{ revset: string; opts: Partial<RangeOpen>; fileset?: string; message: string }>();
  const openRange = (revset: string, opts: Partial<RangeOpen> & { allowNonChain?: boolean }, fileset?: string) => {
    setRange(undefined);
    setAction(undefined);
    return engine.current!
      .openRange(revset, opts, fileset)
      .then((ok: boolean) => ok && setRangeView(revset))
      .catch((x) => {
        // 400 range-not-chain (D16): say why, and offer the commits per commit instead.
        if ((x as { code?: string }).code === "range-not-chain" && !opts.allowNonChain) setNonChain({ revset, opts, fileset, message: (x as Error).message });
        else setToast(`Could not open the range: ${(x as Error).message}`);
        throw x;
      });
  };
  /**
   * Workspaces "Compare" (D12): POST /workspaces:compare {a, b} answers a range view of both stacks
   * per commit on their shared files. A server without that route gets the same range spelled out.
   */
  const compare = async (a: string, b: string, paths: string[]) => {
    const r = await fetch("/api/v1/workspaces:compare", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ a, b }) }).catch(() => undefined);
    const body = r ? await r.json().catch(() => ({})) : {};
    if (r?.ok && body?.spec) {
      setAction(undefined);
      await engine.current!.openView(body.spec).catch((x) => setToast(`Could not open the comparison: ${(x as Error).message}`));
      engine.current!.fit(600);
      setRangeView(`${a}@ ⇄ ${b}@`);
      return;
    }
    const err = body?.error ?? body;
    if (r && r.status === 400) return setToast(err?.code === "no-overlap" ? `${a}@ and ${b}@ change no files in common.` : `Could not compare: ${err?.message ?? r.status}`);
    if (r && r.status === 404 && err?.code && err.code !== "not-found") return setToast(`Could not compare: ${err.message}`);
    const fileset = paths.map((p) => `root-file:${JSON.stringify(p)}`).join(" | ") || undefined;
    return openRange(`(trunk()..${a}@) | (trunk()..${b}@)`, { diff: "per-commit", allowNonChain: true }, fileset).catch(() => openRange(`${a}@ | ${b}@`, { diff: "per-commit", allowNonChain: true }, fileset).catch(() => {}));
  };
  /**
   * A snapshot at another revision or workspace (DESIGN §7): from the revision chip, a history row or
   * a workspace card. Edits there follow §6 for that commit. Rejects with the server's message.
   */
  const openSnapshotAt = async (rev: string, options?: SnapshotOptions) => {
    setAction(undefined);
    await engine.current!.openSnapshot(rev, options);
    setRangeView(undefined);
  };
  const pickLens = (l: Lens) => {
    setLens(l);
    engine.current?.setLens(l);
  };

  const theme = dark ? DARK : LIGHT;
  return (
    <div className="app">
      <header className="top">
        <h1>Code Viewer</h1>
        <div className="seg" role="group" aria-label="View mode">
          <button className={rangeView ? "" : "on"} onClick={() => rangeView && engine.current?.backToSnapshot().then(() => setRangeView(undefined))}>
            Snapshot
          </button>
          <button className={rangeView ? "on" : ""} onClick={() => setRange(rangeView ?? "trunk()..@")}>
            Range
          </button>
        </div>
        <span className="revpick-anchor">
          <button
            className={`chip mono rev-chip ${snap.rev !== "@" ? "away" : ""}`}
            title={`Snapshot of ${snap.rev}${snap.head ? ` (${snap.head.slice(0, 12)})` : ""}${snap.workspace ? ` with ${snap.workspace}'s live files` : ""}: open another revision or workspace`}
            aria-haspopup="dialog"
            aria-expanded={revPicker}
            onClick={() => setRevPicker((o) => !o)}
          >
            {snap.rev}
          </button>
          {revPicker && engine.current && <RevisionPicker current={snap.rev} onOpen={openSnapshotAt} onClose={() => setRevPicker(false)} />}
        </span>
        {engine.current && (
          <SearchBox
            engine={engine.current}
            onThread={(id) => {
              const t = review.threads.get(id);
              const w = t && engine.current?.client.windowsByPath.get(t.anchor.path)?.find((w) => t.anchor.start >= w.lines.start && t.anchor.start <= w.lines.end);
              if (t && w) setAction({ type: "thread", thread: t, window: w });
            }}
          />
        )}
        {rangeView && (
          <span className="chip range">
            range <span className="mono">{rangeView}</span>
            <button className="x" title="Back to the snapshot" onClick={() => (setAction(undefined), engine.current?.backToSnapshot().then(() => setRangeView(undefined)))}>
              ×
            </button>
          </span>
        )}
        <div className="grow" />
        {agent && Date.now() - agent.at < 60_000 && (
          <button className="chip agent-chip" onClick={() => setTab("agent")} title="Agent activity">
            <i /> {agent.name} · {agent.tool}
          </button>
        )}
        <span className={`live ${liveStatus}`} title={`events: ${liveStatus}`} />
        <button className="ghost drawer-btn" onClick={() => setDrawer((d) => !d)} aria-expanded={drawer}>
          Panels
        </button>
        <button className="ghost" onClick={() => setThemePref(dark ? "light" : "dark")} title="Toggle theme">
          {dark ? "Light" : "Dark"}
        </button>
      </header>
      <div className="legend">
        <span className="muted">Links</span>
        {LINK_GROUPS.map((g) => (
          <button key={g} className={`pill ${hidden.has(g) ? "off" : ""}`} aria-pressed={!hidden.has(g)} onClick={() => toggleGroup(g)}>
            <i className={`stroke ${g}`} style={{ color: hex(edgeColor(theme, GROUP_KIND[g])) }} />
            {GROUP_LABEL[g]}
          </button>
        ))}
        <span className="muted sep">Lens</span>
        <div className="seg small" role="group" aria-label="Lens">
          {LENSES.map(([l, label]) => (
            <button key={l} className={lens === l ? "on" : ""} onClick={() => pickLens(l)}>
              {label}
            </button>
          ))}
        </div>
        <span className="muted sep">Lanes</span>
        {boot?.laneDefs.map((l) => (
          <button
            key={l.id}
            className={`pill ${hiddenLanes.has(l.id) ? "off" : ""}`}
            aria-pressed={!hiddenLanes.has(l.id)}
            onClick={() => {
              const next = new Set(hiddenLanes);
              if (next.has(l.id)) next.delete(l.id);
              else next.add(l.id);
              setHiddenLanes(next);
              engine.current?.setHiddenLanes(next);
            }}
          >
            {l.label}
          </button>
        ))}
        <div className="grow" />
        <span className="muted">Heat</span>
        {(() => {
          const labels = boot?.heatLabels ?? ["Frozen", "Cool", "Mild", "Warm", "Hot"];
          // One continuous ramp: windows and clusters sit anywhere along it, not on five swatches.
          const stops = theme.heatRamp.map((c, i) => `${hex(c)} ${((i / (theme.heatRamp.length - 1)) * 100).toFixed(1)}%`).join(", ");
          return (
            <span className="heatbar" title={labels.join(" → ")}>
              {labels[0]}
              <i style={{ background: `linear-gradient(90deg, ${stops})` }} />
              {labels[4]}
            </span>
          );
        })()}
      </div>
      <main>
        <div className="canvas" ref={host}>
          {error && (
            <div className="error">
              Could not open the view: {error}. {/unauthori|token/i.test(error) ? "Open the UI link `cv serve` printed: it carries the access token." : "Is `cv serve` running?"}
            </div>
          )}
          {toast && (
            <div className="toast" role="status">
              {toast}
            </div>
          )}
          <div className="zoomctl">
            <button onClick={() => engine.current?.cam.zoomAt(engine.current.cam.width / 2, engine.current.cam.height / 2, 1.5)}>+</button>
            <button onClick={() => engine.current?.cam.zoomAt(engine.current.cam.width / 2, engine.current.cam.height / 2, 1 / 1.5)}>−</button>
            <button onClick={() => engine.current?.fit()}>Fit</button>
            <button onClick={() => engine.current?.home()} title="Review first (h)">⌂</button>
          </div>
          {engine.current && action?.type === "thread" && (
            <ThreadPopover key={action.thread.id} engine={engine.current} review={review} threadId={action.thread.id} w={action.window} onClose={() => setAction(undefined)} />
          )}
          {engine.current && action?.type === "compose" && (
            <Composer
              key={`${action.window.id}:${action.line}`}
              engine={engine.current}
              review={review}
              w={action.window}
              line={action.line}
              end={action.end}
              onClose={() => setAction(undefined)}
              onCreated={(t) => setAction({ type: "thread", thread: t, window: action.window })}
            />
          )}
          {suggestions.length > 0 && (
            <div className="suggestions" role="status">
              {suggestions.map((x) => (
                <div key={x.id} className="suggest">
                  <span>
                    <b>{x.by}</b> {x.text}
                    {x.note && <span className="muted"> — {x.note}</span>}
                  </span>
                  <button
                    className="primary"
                    onClick={() => {
                      x.go();
                      setSuggestions((l) => l.filter((y) => y.id !== x.id));
                    }}
                  >
                    Go
                  </button>
                  <button className="x" aria-label="Dismiss" onClick={() => setSuggestions((l) => l.filter((y) => y.id !== x.id))}>
                    ×
                  </button>
                </div>
              ))}
            </div>
          )}
          {engine.current && <UndoBar engine={engine.current} review={review} />}
          {engine.current && action?.type === "conflict" && (
            <ConflictPopover
              key={action.conflict.path}
              engine={engine.current}
              review={review}
              live={live}
              d={action.conflict}
              w={action.window}
              onClose={() => setAction(undefined)}
              onThread={(t) => setAction({ type: "thread", thread: t, window: action.window })}
            />
          )}
          {engine.current && tour && <TourPlayer key={tour.id} engine={engine.current} tour={tour} onClose={() => setTour(undefined)} />}
          {engine.current && action?.type === "edit" && <Editor key={action.window.id} engine={engine.current} review={review} w={action.window} onClose={() => setAction(undefined)} />}
        </div>
        <aside className={`side ${drawer ? "open" : ""}`}>
          <nav className="tabs">
            {TABS.map(([k, l]) => (
              <button key={k} className={tab === k ? "on" : ""} onClick={() => setTab(k)}>
                {l}
              </button>
            ))}
          </nav>
          {engine.current && tab === "inspector" && <Inspector key={sel.v} engine={engine.current} review={review} sel={sel.t} onAction={setAction} />}
          {engine.current && tab === "history" && <HistoryPanel engine={engine.current} live={live} onRange={setRange} onInterdiff={setInterdiff} onSnapshot={(rev) => openSnapshotAt(rev).catch((x) => setToast(`Could not open ${rev.slice(0, 12)}: ${(x as Error).message}`))} />}
          {engine.current && tab === "review" && <ReviewPanel engine={engine.current} review={review} onOpen={(t) => openThread(t)} />}
          {engine.current && tab === "workspaces" && <WorkspacesPanel engine={engine.current} live={live} onRange={setRange} onCompare={compare} onOpenAt={(rev, options) => openSnapshotAt(rev, options).catch((x) => { setToast(`Could not open ${rev}: ${(x as Error).message}`); throw x; })} />}
          {engine.current && tab === "hotspots" && <HotspotsPanel engine={engine.current} />}
          {engine.current && tab === "agent" && <AgentPanel engine={engine.current} live={live} onOpen={(t) => openThread(t)} follow={followAgent} setFollow={setFollowAgent} onTour={setTour} />}
        </aside>
      </main>
      {range !== undefined && (
        <RangeDialog
          initial={range}
          onClose={() => setRange(undefined)}
          onOpen={(revset, opts, fileset) => void openRange(revset, opts, fileset).catch(() => {})}
        />
      )}
      {nonChain && (
        <div className="modal-back" onClick={() => setNonChain(undefined)}>
          <div className="modal" role="alertdialog" aria-label="Not a chain" onClick={(e) => e.stopPropagation()}>
            <h2>Not a chain</h2>
            <p className="small">{nonChain.message}</p>
            <div className="row end">
              <button onClick={() => (setNonChain(undefined), setRange(nonChain.revset))}>Edit the revset</button>
              <button
                className="primary"
                onClick={() => {
                  const n = nonChain;
                  setNonChain(undefined);
                  openRange(n.revset, { ...n.opts, diff: "per-commit", allowNonChain: true }, n.fileset).catch(() => {});
                }}
              >
                Open per commit anyway
              </button>
            </div>
          </div>
        </div>
      )}
      {interdiff && <InterdiffViewer target={interdiff} onClose={() => setInterdiff(undefined)} />}
      <footer className="status mono">
        {stats ? (
          <>
            <span>
              {stats.tier} {TIER_NAMES[stats.tier]} · {(stats.zoom * 100).toFixed(stats.zoom < 0.1 ? 1 : 0)}%
            </span>
            <span>
              {stats.windowsVisible.toLocaleString()} / {stats.windowsLoaded.toLocaleString()} / {stats.windowsTotal.toLocaleString()} windows
            </span>
            <span>{stats.pendingRequests ? `loading ${stats.pendingRequests}` : "idle"}</span>
            {indexing && (
              <span className="indexing" title="the server is indexing a new snapshot">
                indexing {indexing.job} {indexing.total > 1 ? `${Math.round((100 * indexing.done) / Math.max(1, indexing.total))}%` : "…"}
              </span>
            )}
            <span className="grow" />
            {(boot?.churn ?? repoChurn) && (
              <span className="churn" title="change frequency: commits scanned and the share of change mass in files whose heat grade has settled (§4.7)">
                {churnText((boot?.churn ?? repoChurn)!)}
              </span>
            )}
            <span title="frame interval p50 / p95 / max over the last 240 frames of navigation">
              {stats.fps.toFixed(0)} fps · p95 {stats.frameP95.toFixed(1)} ms · max {stats.frameMax.toFixed(0)} ms
            </span>
            <span title="chunk rebuild time in the last second">build {stats.buildMsLastSecond.toFixed(0)} ms/s</span>
            <span title={boot?.view.resolved.op}>op {boot?.view.resolved.op.slice(0, 12)}</span>
          </>
        ) : (
          <span>opening view…</span>
        )}
      </footer>
    </div>
  );
}

/** Resolves once the UI and code fonts are loaded, or after 2.5 s, whichever comes first. */
function fontsReady(): Promise<unknown> {
  if (!document.fonts) return Promise.resolve();
  const load = Promise.all(['600 48px Inter', '500 48px "IBM Plex Mono"'].map((f) => document.fonts.load(f).catch(() => undefined)));
  return Promise.race([load, new Promise((r) => setTimeout(r, 2500))]);
}
