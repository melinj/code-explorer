import { useEffect, useState } from "react";
import type { CanvasAction, Engine } from "../canvas/engine";
import type { ReviewClient } from "../data/review";
import { GROUP_OF, edgeColor, isUncertain } from "../canvas/edges";
import { CONN_NAMES, HEAT_NAMES, countByGroup, coveredText } from "../canvas/describe";
import { inRangeText, ownerColor, stubText } from "../canvas/chunks";
import type { Target } from "../canvas/overlay";
import { hex } from "../canvas/theme";
import type { Edge } from "../data/client";
import type { Cluster, Link } from "../contracts/model";
import { LocalLine } from "./code";
import { Linked } from "./Linked";

function LinkRow({ engine, e, from }: { engine: Engine; e: Edge; from?: string }) {
  const c = engine.client;
  const outgoing = from ? e.from === from : true;
  const otherId = outgoing ? e.to : e.from;
  const other = c.windows.get(otherId);
  const color = hex(edgeColor(engine.theme, e.kind));
  // The line of the selected window this link leaves from (or arrives at), shown and peeked.
  const self = from ? c.windows.get(from) : undefined;
  const line = self ? engine.lineAt(self, outgoing ? e.ay : e.by) : undefined;
  const peek = (on: boolean) => engine.peek(on ? { w: self, line, edge: e } : undefined);
  return (
    <li style={{ borderLeftColor: color }} className="linkrow">
      <button
        className="link"
        onClick={() => (engine.peek(undefined), engine.follow(e, from ?? e.from))}
        onMouseEnter={() => peek(true)}
        onMouseLeave={() => peek(false)}
        onFocus={() => peek(true)}
        onBlur={() => peek(false)}
        title="Follow this link"
      >
        <i style={{ background: color }} />
        <span className="kind">{e.kind}</span>
        <span className="name">
          {other ? (other.symbol?.qualname ?? other.path) : ((outgoing ? e.toLabel : e.fromLabel) ?? siteName(c.links.get(e.linkId), outgoing) ?? `${otherId} (not loaded)`)}
          {isUncertain(e) && <span className="dim" title="low confidence"> ?</span>}
        </span>
        <span className="path">{other ? other.path.split("/").pop() : ""}</span>
        {self && line !== undefined && <LocalLine engine={engine} self={self} line={line} />}
      </button>
    </li>
  );
}

/** Name of a link's far end from its Link record (symbol, else path:line), when its window isn't loaded. */
function siteName(l: Link | undefined, outgoing: boolean) {
  if (!l) return undefined;
  const site = outgoing ? l.to : l.from;
  if ("dynamic" in site) return stubText(site.dynamic);
  if (!("path" in site)) return l.label;
  return site.symbol ?? `${site.path.split("/").pop()}:${site.line}`;
}

/** Details and links of the selection; links are buttons that follow them on the canvas. */
export function Inspector({ engine, review, sel, onAction }: { engine: Engine; review: ReviewClient; sel?: Target; onAction: (a: CanvasAction) => void }) {
  const [, force] = useState(0);
  useEffect(() => review.onChange(() => force((n) => n + 1)), [review]);
  if (!sel) {
    return (
      <div className="pad muted">
        <p>Click a window to show only its links, a cluster box or a highway to unbundle it, or a link to follow it.</p>
        <p>Drag to pan, wheel to zoom. f fits the map, h returns to review-first, Esc clears the selection.</p>
      </div>
    );
  }
  const edges = engine.edgesOf(sel);
  const head = (title: string, sub?: string) => (
    <header className="ins-head">
      <h2>{title}</h2>
      {sub && <div className="mono muted">{sub}</div>}
      <div className="row">
        <button onClick={() => engine.focusTarget(sel)}>Frame</button>
        <button onClick={() => engine.select(undefined)}>Clear</button>
      </div>
    </header>
  );
  switch (sel.type) {
    case "window": {
      const w = sel.w;
      // In line order, so the lists read like the window: each row shows its line.
      const out = edges.filter((e) => e.from === w.id).sort((a, b) => a.ay - b.ay);
      const inc = edges.filter((e) => e.from !== w.id).sort((a, b) => a.by - b.by);
      return (
        <div className="ins">
          {head(w.symbol?.qualname ?? w.path, `${w.path}:${w.lines.start}–${w.lines.end}`)}
          <dl>
            <dt>Heat</dt>
            <dd>
              {HEAT_NAMES[w.heat.grade]} · {w.heat.touches} touches
            </dd>
            <dt>Depends</dt>
            <dd>
              {CONN_NAMES[w.conn.grade]} · ↧ {w.conn.dependents2} · ↥ {w.conn.fanOut}
            </dd>
            <dt>Tests</dt>
            <dd>{coveredText(w)}</dd>
            {w.heat.inRange && (
              <>
                <dt>Range</dt>
                <dd className="accent">{inRangeText(w)}</dd>
              </>
            )}
            {(w.flags.alsoChangedIn?.length || w.flags.unsnapshotted) && (
              <>
                <dt>Live</dt>
                <dd>
                  {w.flags.unsnapshotted && <span className="add">edited, not snapshotted </span>}
                  {w.flags.alsoChangedIn?.length ? <span className="warn">also changed in {w.flags.alsoChangedIn.map((x) => x + "@").join(", ")}</span> : null}
                </dd>
              </>
            )}
            {w.fragment && (
              <>
                <dt>File</dt>
                <dd>
                  fragment {w.fragment.index} of {w.fragment.of}
                </dd>
              </>
            )}
            {w.reviewFirst && (
              <>
                <dt>Risk</dt>
                <dd className="accent">review first · {(w.risk * 100).toFixed(0)}%</dd>
              </>
            )}
          </dl>
          <Owners engine={engine} id={w.id} />
          <PinRow engine={engine} pinned={(w.flags as { pinned?: boolean }).pinned === true} hint="Drag its title bar to move it; it stays pinned there." onUnpin={() => engine.moveTo(w.key, w.rect.x, w.rect.y, false)} />
          <div className="row">
            <button onClick={() => onAction({ type: "compose", window: w, line: w.lines.start })}>Comment</button>
            <button onClick={() => (engine.focusTarget(sel), onAction({ type: "edit", window: w }))}>Edit</button>
          </div>
          {(() => {
            const threads = review.threadsIn(w);
            return (
              <>
                <h3>Threads · {threads.length}</h3>
                <ul className="links">
                  {threads.map((t) => (
                    <li key={t.id}>
                      <button className="link thread" onClick={() => onAction({ type: "thread", thread: t, window: w })}>
                        <i className={`st ${t.status}`} />
                        <span className="kind">L{t.anchor.start}</span>
                        <span className="name">{t.comments[0]?.body ?? t.kind}</span>
                        <span className="path">{t.status}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              </>
            );
          })()}
          <Linked engine={engine} w={w} />
          <h3>Outgoing · {out.length}</h3>
          <ul className="links">{out.map((e) => <LinkRow key={e.id} engine={engine} e={e} from={w.id} />)}</ul>
          <h3>Incoming · {inc.length}</h3>
          <ul className="links">{inc.map((e) => <LinkRow key={e.id} engine={engine} e={e} from={w.id} />)}</ul>
        </div>
      );
    }
    case "cluster": {
      const k = sel.c;
      const hws = engine.highwaysOf(k.id);
      return (
        <div className="ins">
          {head(k.label, `${k.windows} windows · ${k.lane} lane`)}
          <dl>
            <dt>Heat</dt>
            <dd>{HEAT_NAMES[k.heat]}</dd>
            <dt>Depends</dt>
            <dd>{CONN_NAMES[k.conn]}</dd>
            <dt>Links</dt>
            <dd>{countByGroup(edges) || "none loaded"}</dd>
          </dl>
          <PinRow engine={engine} pinned={(k as { pinned?: boolean }).pinned === true} hint="Zoomed out, drag it to move it; it stays pinned there." onUnpin={() => engine.moveTo(`cluster:${k.id}`, k.rect.x, k.rect.y, false)} />
          <TestLinks engine={engine} c={k} />
          <h3>Highways · {hws.length}</h3>
          <ul className="links">
            {hws.slice(0, 80).map((h) => {
              const out = h.from.id === k.id;
              return (
                <li key={h.key}>
                  <button className="link" onClick={() => (engine.select({ type: "highway", h }), engine.focusTarget({ type: "highway", h }))}>
                    <i style={{ background: hex(edgeColor(engine.theme, h.kind)) }} />
                    <span className="kind">{out ? "→" : "←"} {h.count}</span>
                    <span className="name">{(out ? h.to : h.from).label}</span>
                    <span className="path">{(out ? h.to : h.from).lane}</span>
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      );
    }
    case "highway": {
      const h = sel.h;
      return (
        <div className="ins">
          {head(`${h.from.label} → ${h.to.label}`, `${h.bundle.weight} links`)}
          <p className="pad0 muted">{countByGroup(edges) || "Links load as you pan near either cluster."}</p>
          <ul className="links">{edges.map((e) => <LinkRow key={e.id} engine={engine} e={e} />)}</ul>
        </div>
      );
    }
    case "edge":
      return (
        <div className="ins">
          {head(sel.e.kind, GROUP_OF[sel.e.kind])}
          <ul className="links">
            <LinkRow engine={engine} e={sel.e} />
          </ul>
        </div>
      );
    case "hub": {
      const h = sel.h;
      const color = (k: number) => hex(edgeColor(engine.theme, edges[k]?.kind ?? "publish"));
      const sites = (list: typeof h.producers, title: string, offset: number) => (
        <>
          <h3>
            {title} · {list.length}
          </h3>
          <ul className="links">
            {list.slice(0, 200).map((s, i) => (
              <li key={`${s.path}:${s.line}:${i}`}>
                <button className="link" onClick={() => engine.goTo(s.window && engine.client.windows.has(s.window) ? { window: s.window } : { path: s.path, line: s.line })} title="Fly there">
                  <i style={{ background: color(offset + i) }} />
                  <span className="name">{s.path.split("/").pop()}</span>
                  <span className="path">:{s.line}</span>
                </button>
              </li>
            ))}
          </ul>
        </>
      );
      const [pin, pout] = h.protocol === "http" ? ["Clients", "Routes"] : h.protocol === "bus" ? ["Publishers", "Subscribers"] : ["Defined by", "Read by"];
      return (
        <div className="ins">
          {head(h.display, `${h.protocol} hub${h.family ? ` · ${h.family}` : ""}`)}
          {sites(h.producers, pin, 0)}
          {sites(h.consumers, pout, h.producers.length)}
        </div>
      );
    }
    case "expand":
      return null;
    case "conflict":
      return <div className="ins">{head(`conflict in ${sel.h.conflict.path.split("/").pop()}`, sel.h.conflict.path)}</div>;
    case "thread":
      return <div className="ins">{head(`${sel.h.thread.kind} · ${sel.h.thread.status}`, `${sel.h.thread.anchor.path}:${sel.h.thread.anchor.start}`)}</div>;
    case "port":
      return (
        <div className="ins">
          {head(sel.p.label, `${sel.p.window.path}:${sel.p.line}`)}
          <ul className="links">{edges.map((e) => <LinkRow key={e.id} engine={engine} e={e} from={sel.p.window.id} />)}</ul>
        </div>
      );
  }
}

/**
 * Tests and the code they exercise, at cluster level: for a code cluster the test clusters that
 * link into it, for a test cluster the code clusters it links to (bootstrap bundles, by weight).
 */
function TestLinks({ engine, c }: { engine: Engine; c: Cluster }) {
  const boot = engine.client.boot;
  const byId = new Map(boot.clusters.map((x) => [x.id, x]));
  const isTest = c.lane === "test";
  const rows = boot.bundles
    .filter((b) => (isTest ? b.from === c.id && byId.get(b.to)?.lane !== "test" : b.to === c.id && byId.get(b.from)?.lane === "test"))
    .map((b) => ({ other: byId.get(isTest ? b.to : b.from)!, weight: b.weight }))
    .filter((r) => r.other)
    .sort((a, b) => b.weight - a.weight)
    .slice(0, 8);
  if (!rows.length) return null;
  return (
    <>
      <h3>
        {isTest ? "Tested code" : "Tests"} · {rows.length}
      </h3>
      <ul className="links">
        {rows.map(({ other, weight }) => (
          <li key={other.id}>
            <button className="link" onClick={() => (engine.select({ type: "cluster", c: other }), engine.focusTarget({ type: "cluster", c: other }))} title="Fly there">
              <i style={{ background: hex(edgeColor(engine.theme, "call")) }} />
              <span className="kind">{weight}</span>
              <span className="name">{other.label}</span>
              <span className="path">{other.lane}</span>
            </button>
          </li>
        ))}
      </ul>
    </>
  );
}

/** Pinned state of a window or cluster: "Pinned · Unpin", else how to move it. */
function PinRow({ engine, pinned, hint, onUnpin }: { engine: Engine; pinned: boolean; hint: string; onUnpin: () => void }) {
  if (engine.isRange) return null;
  return pinned ? (
    <div className="row pin">
      <span className="accent">Pinned</span>
      <button onClick={onUnpin} title="Let the layout place it again">
        Unpin
      </button>
    </div>
  ) : (
    <p className="muted small pad0">{hint}</p>
  );
}

/** Range views: which commits own this hunk's lines (the stripe colours on the canvas). */
function Owners({ engine, id }: { engine: Engine; id: string }) {
  const d = engine.client.diffs.get(id);
  if (!engine.isRange || !d) return null;
  const n = new Map<string, number>();
  for (const l of d.lines) if (l.owner && l.kind !== "ctx") n.set(l.owner, (n.get(l.owner) ?? 0) + 1);
  if (!n.size) return null;
  return (
    <div className="owners small">
      {[...n].map(([o, k]) => (
        <span key={o} className="owner">
          <i style={{ background: hex(ownerColor(engine.client.ownerSlot(o))) }} />
          <span className="mono">{o.slice(0, 8)}</span> <span className="muted">{k} lines</span>
        </span>
      ))}
    </div>
  );
}
