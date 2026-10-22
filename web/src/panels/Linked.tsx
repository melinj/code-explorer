import { useEffect, useState } from "react";
import type { Link, LinkKind, SiteRef, WindowHeader } from "../contracts/model";
import type { Engine } from "../canvas/engine";
import { edgeColor } from "../canvas/edges";
import { hex } from "../canvas/theme";
import { CodeLine, LocalLine, useText } from "./code";

/** One linked window (or hub) under a relation, with the lines the links touch there. */
interface Entry {
  key: string;
  kind: LinkKind;
  w?: WindowHeader;
  site?: SiteRef;
  label: string;
  /** Linked lines at the far end, and the lines of the selected window the links leave from. */
  lines: number[];
  local: number[];
  linkIds: string[];
}

type Group = { title: string; entries: Entry[] };

const CHANNEL: ReadonlySet<string> = new Set(["publish", "subscribe", "dispatch", "http-request", "http-route", "config", "define", "read", "deploy"]);

/** The relation a link has from `w`'s side: outgoing or incoming, by kind, with tests apart. */
function relation(kind: LinkKind, out: boolean, self: WindowHeader, other?: WindowHeader): string {
  if (CHANNEL.has(kind)) return "Channels";
  const selfTest = self.lane === "test";
  const otherTest = other?.lane === "test";
  if (!selfTest && otherTest) return "Tests";
  if (selfTest && other && !otherTest) return "Tested code";
  if (kind === "inherit") return out ? "Base classes" : "Subclasses";
  if (kind === "implement") return out ? "Implements" : "Implementations";
  if (kind === "call") return out ? "Calls" : "Called by";
  return out ? "Uses" : "Used by";
}

const ORDER = ["Tests", "Tested code", "Calls", "Called by", "Base classes", "Subclasses", "Implements", "Implementations", "Uses", "Used by", "Channels"];

function group(self: WindowHeader, links: Link[], headers: Map<string, WindowHeader>): Group[] {
  const groups = new Map<string, Map<string, Entry>>();
  for (const l of links) {
    const out = l.from.window === self.id;
    const far = out ? l.to : l.from;
    const site = "path" in far ? (far as SiteRef) : undefined;
    // Links within the window (a local call) aren't "linked code".
    if (site?.window === self.id) continue;
    const w = site?.window ? headers.get(site.window) : undefined;
    const title = relation(l.kind, out, self, w);
    const key = w?.id ?? site?.path ?? l.label;
    const line = site?.line ?? 0;
    const near = out ? l.from.line : "path" in l.to ? (l.to as SiteRef).line : 0;
    const g = groups.get(title) ?? new Map<string, Entry>();
    groups.set(title, g);
    const e = g.get(key) ?? { key, kind: l.kind, w, site, label: w?.symbol?.qualname ?? (site ? site.path : l.label), lines: [], local: [], linkIds: [] };
    g.set(key, e);
    if (line && !e.lines.includes(line)) e.lines.push(line);
    if (near && !e.local.includes(near)) e.local.push(near);
    e.linkIds.push(l.id);
  }
  return ORDER.filter((t) => groups.has(t)).map((t) => ({ title: t, entries: [...groups.get(t)!.values()] }));
}

/** A few lines of the far end around its first linked line; that line tinted in the link's colour. */
function Preview({ engine, w, line, color }: { engine: Engine; w: WindowHeader; line: number; color: string }) {
  const from = Math.max(w.lines.start, line - 2);
  const to = Math.min(w.lines.end, from + 6);
  const text = useText(engine, w, from, to);
  if (!text) return <pre className="lk-code muted">…</pre>;
  const dedent = Math.min(...text.lines.filter((s) => s.trim()).map((s) => s.length - s.trimStart().length), 80);
  return (
    <pre className="lk-code">
      {text.lines.map((_, i) => {
        const n = text.from + i;
        return (
          <div key={n} className={n === line ? "hl" : ""} style={n === line ? { boxShadow: `inset 3px 0 0 ${color}` } : undefined}>
            <i>{n}</i>
            <CodeLine engine={engine} text={text} n={n} dedent={dedent} />
          </div>
        );
      })}
    </pre>
  );
}

/**
 * Linked code for the selected window (a section of the Inspector): its tests (or the code a test exercises), callers and
 * callees, base classes and subclasses, and channels, each with a short preview of the far end.
 * Ends that aren't loaded on the canvas come from the links route's headers; click flies there.
 */
export function Linked({ engine, w }: { engine: Engine; w: WindowHeader }) {
  const [groups, setGroups] = useState<Group[]>();
  const [err, setErr] = useState<string>();
  const [more, setMore] = useState<Set<string>>(new Set());
  useEffect(() => {
    let live = true;
    setGroups(undefined);
    setErr(undefined);
    const q = new URLSearchParams({ viewId: engine.client.view.id, target: w.id, dir: "both", depth: "1", headers: "1" });
    fetch(`/api/v1/links?${q}`)
      .then(async (r) => {
        if (!r.ok) throw new Error(`links: ${r.status}`);
        const res = (await r.json()) as { links: Link[]; windows?: WindowHeader[] };
        const headers = new Map<string, WindowHeader>();
        for (const h of res.windows ?? []) headers.set(h.id, h);
        for (const [id, h] of engine.client.windows) if (!headers.has(id)) headers.set(id, h);
        if (live) setGroups(group(w, res.links, headers));
      })
      .catch((e) => live && setErr(String(e.message ?? e)));
    return () => void (live = false);
  }, [engine, w]);
  // Hovering a card points at its line in the selected window and highlights the link.
  const peek = (e?: Entry) => {
    if (!e || !e.local.length) return engine.peek(undefined);
    const edge = engine.edgesOf({ type: "window", w }).find((x) => e.linkIds.includes(x.linkId));
    engine.peek({ w, line: e.local[0], edge });
  };
  useEffect(() => () => engine.peek(undefined), [engine, w]);
  const go = (e: Entry) => {
    engine.peek(undefined);
    const line = e.lines[0];
    if (e.w) engine.goTo({ path: e.w.path, line: line ?? e.w.lines.start });
    else if (e.site) engine.goTo({ path: e.site.path, line: e.site.line });
  };
  const total = groups?.reduce((n, g) => n + g.entries.length, 0) ?? 0;
  return (
    <section className="linked" aria-label="Linked code">
      <h3 className="lk-title">Linked code{groups ? ` · ${total}` : ""}</h3>
      {err && <div className="err">{err}</div>}
      {!groups && !err && <div className="muted pad">Loading…</div>}
      {groups && !total && <div className="muted pad">No linked code.</div>}
      {groups?.map((g) => {
        const shown = more.has(g.title) ? g.entries : g.entries.slice(0, 6);
        return (
          <div key={g.title} className="lk-group">
            <h3>
              {g.title} · {g.entries.length}
            </h3>
            {shown.map((e) => (
              <button
                key={e.key}
                className="lk-card"
                style={{ borderLeftColor: hex(edgeColor(engine.theme, e.kind)) }}
                onClick={() => go(e)}
                onMouseEnter={() => peek(e)}
                onMouseLeave={() => peek(undefined)}
                onFocus={() => peek(e)}
                onBlur={() => peek(undefined)}
                title="Fly there"
              >
                <div className="lk-head">
                  <span className="kind" style={{ color: hex(edgeColor(engine.theme, e.kind)) }}>
                    {e.kind}
                  </span>
                  <span className="name">{e.label}</span>
                  <span className="path mono">
                    {(e.w?.path ?? e.site?.path ?? "").split("/").pop()}
                    {e.lines.length ? `:${e.lines[0]}` : ""}
                    {e.lines.length > 1 ? ` +${e.lines.length - 1}` : ""}
                  </span>
                </div>
                {e.local.length > 0 && <LocalLine engine={engine} self={w} line={e.local[0]} more={e.local.length - 1} />}
                {e.w && <Preview engine={engine} w={e.w} line={e.lines[0] ?? e.w.lines.start} color={hex(edgeColor(engine.theme, e.kind))} />}
              </button>
            ))}
            {g.entries.length > shown.length && (
              <button className="linkish small" onClick={() => setMore(new Set(more).add(g.title))}>
                Show {g.entries.length - shown.length} more
              </button>
            )}
          </div>
        );
      })}
    </section>
  );
}
