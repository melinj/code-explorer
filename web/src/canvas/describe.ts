import type { Edge, DataClient } from "../data/client";
import { GROUP_OF, LINK_GROUPS, isUncertain, type LinkGroup } from "./edges";
import type { Target } from "./overlay";
import type { WindowHeader } from "../contracts/model";
import { inRangeText } from "./chunks";
import { dirOf, structure } from "./structure";

/** "covered by 3 tests" (§4.9: test dependents are named, not counted into connectedness). */
export function coveredText(w: WindowHeader) {
  const n = w.conn.tests;
  return n ? `covered by ${n} test${n === 1 ? "" : "s"}` : "no tests link here";
}

export const HEAT_NAMES = ["Frozen", "Cool", "Mild", "Warm", "Hot"];
export const CONN_NAMES = ["Isolated", "Low", "Medium", "High", "Core"];
export const GROUP_LABEL: Record<LinkGroup, string> = { calls: "calls", inherit: "inherit", events: "events", http: "HTTP", config: "config" };

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;");

export function countByGroup(edges: Edge[]) {
  const n: Partial<Record<LinkGroup, number>> = {};
  for (const e of edges) n[GROUP_OF[e.kind]] = (n[GROUP_OF[e.kind]] ?? 0) + 1;
  return LINK_GROUPS.filter((g) => n[g]).map((g) => `${n[g]} ${GROUP_LABEL[g]}`).join(" · ");
}

function name(c: DataClient, id: string) {
  const w = c.windows.get(id);
  return w ? esc(w.symbol?.qualname ?? w.path.split("/").pop()!) : `<span class="dim">${esc(id)} (not loaded)</span>`;
}

const pctOf = (x: number) => `${Math.round(x * 100)}%`;

/**
 * A directory's figures as a module (DESIGN §4.11), for a folder-tree lens: links staying in it,
 * how many of its windows are reached from outside, which files carry its outside links, its
 * longest inner chain and its heat tension.
 */
function folderRows(folder: string | undefined): string[] {
  const f = folder === undefined ? undefined : structure?.folders.get(folder);
  if (!f) return [];
  const touching = f.internal + f.outgoing + f.incoming;
  const rows = [`<div class="mono dim">${esc(f.path || "(root)")}/ as a module</div>`];
  if (touching) {
    rows.push(`<div><span class="k">Depth</span> ${pctOf(f.inside)} of ${touching} links stay inside · ${f.outgoing} out · ${f.incoming} in · ${f.partners} partner folder${f.partners === 1 ? "" : "s"}</div>`);
    rows.push(`<div><span class="k">Interface</span> ${f.interface} of ${f.windows} windows exposed · ${f.gateways} of ${f.files} files are gateways (spread ${pctOf(f.spread)}) · chain ${f.chain}</div>`);
  } else rows.push(`<div class="dim">no links touch it</div>`);
  if (f.tension > 0) rows.push(`<div><span class="k">Tension</span> ${pctOf(f.tension)}${f.hotFiles.length ? ` · busy: ${f.hotFiles.map((h) => esc(h.split("/").pop()!)).join(", ")}` : ""}</div>`);
  return rows;
}

/** A window's figures against its directory, its file's tension and pull, then the directory's. */
function windowStructureRows(w: WindowHeader): string[] {
  if (!structure) return [];
  const rows: string[] = [];
  const s = structure.windows.get(w.key);
  if (s && s.internal + s.external) {
    rows.push(`<div><span class="k">Module</span> ${s.internal} links inside its folder (or below) · ${s.external} outside (${s.external - s.incoming} out, ${s.incoming} in) · ${pctOf(s.internal / (s.internal + s.external))} inside</div>`);
  }
  const f = structure.files.get(w.path);
  if (f?.tension) rows.push(`<div><span class="k">File tension</span> ${pctOf(f.tension)}: busier than its siblings</div>`);
  if (f?.pull) rows.push(`<div class="accent">pulled toward ${esc(f.pull.folder)}/ (${f.pull.links} links, ${f.internal} with its own folder)</div>`);
  return [...rows, ...folderRows(dirOf(w.path))];
}

/** Tooltip HTML for a hover target. */
export function describe(t: Target, c: DataClient, edges: Edge[]): string {
  switch (t.type) {
    case "window": {
      const w = t.w;
      const out = edges.filter((e) => e.from === w.id).length;
      const rows = [
        `<div class="t">${esc(w.symbol?.qualname ?? w.path)} <span class="dim">${esc(w.symbol?.kind ?? w.kind)}</span></div>`,
        `<div class="mono dim">${esc(w.path)}:${w.lines.start}–${w.lines.end}${w.fragment ? ` · fragment ${w.fragment.index}/${w.fragment.of}` : ""}</div>`,
        `<div><span class="k">Heat</span> ${HEAT_NAMES[w.heat.grade]} · ${w.heat.touches} touches${w.heat.settled ? "" : " · settling"}</div>`,
        `<div><span class="k">Depends</span> ${CONN_NAMES[w.conn.grade]} · ↧ ${w.conn.dependents2} · ↥ ${w.conn.fanOut} · ${coveredText(w)}</div>`,
        `<div><span class="k">Links</span> ${out} out · ${edges.length - out} in${edges.length ? ` · ${countByGroup(edges)}` : ""}</div>`,
      ];
      if (w.heat.inRange) rows.push(`<div><span class="k">Range</span> touched by ${inRangeText(w)}</div>`);
      rows.push(...windowStructureRows(w));
      if (w.flags.alsoChangedIn?.length) rows.push(`<div class="warn">also changed in ${esc(w.flags.alsoChangedIn.map((x) => x + "@").join(", "))}</div>`);
      if (w.flags.unsnapshotted) rows.push(`<div class="add">edited on disk, not yet snapshotted (live)</div>`);
      if (w.reviewFirst) rows.push(`<div class="accent">Review first · risk ${(w.risk * 100).toFixed(0)}% (top 5%)</div>`);
      if (w.counts.openThreads || w.counts.conflicts)
        rows.push(`<div>${w.counts.openThreads ? `${w.counts.openThreads} open threads ` : ""}${w.counts.conflicts ? `<span class="bad">conflict</span>` : ""}</div>`);
      rows.push(`<div class="hint">click to show only its links · double-click to open</div>`);
      return rows.join("");
    }
    case "edge": {
      const e = t.e;
      return [
        `<div class="t">${esc(e.kind)}${isUncertain(e) ? ` <span class="dim">· ${(e.confidence * 100).toFixed(0)}% sure</span>` : ""}</div>`,
        `<div>${name(c, e.from)} <span class="dim">→</span> ${name(c, e.to)}</div>`,
        e.fromCluster !== e.toCluster ? `<div class="dim">across clusters</div>` : "",
        `<div class="hint">click to follow</div>`,
      ].join("");
    }
    case "highway": {
      const h = t.h;
      const kinds = Object.entries(h.bundle.kinds)
        .map(([k, n]) => `${n} ${esc(k)}`)
        .join(" · ");
      return [
        `<div class="t">${esc(h.from.label)} <span class="dim">→</span> ${esc(h.to.label)}</div>`,
        `<div>${h.bundle.weight} links · ${kinds}</div>`,
        `<div class="hint">click to unbundle · double-click to frame both clusters</div>`,
      ].join("");
    }
    case "cluster": {
      const k = t.c;
      return [
        `<div class="t">${esc(k.label)}</div>`,
        `<div>${k.windows} windows · ${esc(k.lane)} lane</div>`,
        `<div><span class="k">Heat</span> ${HEAT_NAMES[k.heat]} · <span class="k">Depends</span> ${CONN_NAMES[k.conn]}</div>`,
        edges.length ? `<div><span class="k">Links</span> ${countByGroup(edges)}</div>` : "",
        k.openThreads ? `<div>${k.openThreads} open threads</div>` : "",
        ...folderRows(k.folder),
        `<div class="hint">click to unbundle its links · double-click to frame</div>`,
      ].join("");
    }
    case "hub":
      return `<div class="t">${esc(t.h.display)}</div><div class="dim">${esc(t.h.protocol)} channel</div>`;
    case "thread": {
      const th = t.h.thread;
      const first = th.comments[0];
      return [
        `<div class="t">${esc(th.kind)} · ${esc(th.status)}${th.assignee ? ` <span class="dim">· for ${esc(th.assignee)}</span>` : ""}</div>`,
        first ? `<div>${esc(first.body.slice(0, 160))}</div>` : "",
        `<div class="dim">${th.comments.length} comment${th.comments.length === 1 ? "" : "s"} · lines ${th.anchor.start}–${th.anchor.end}</div>`,
        `<div class="hint">click to open</div>`,
      ].join("");
    }
    case "expand": {
      const r = t.h.req;
      if (r.collapse) {
        const f = t.h.window.expandedFrom;
        return `<div class="t">Collapse${f ? ` to lines ${f.start}–${f.end}` : ""}</div><div class="hint">back to its size before you expanded it</div>`;
      }
      const dir = r.before !== undefined ? "above" : "below";
      const v = r.before ?? r.after;
      return `<div class="t">Show ${v === "file" ? "every line" : `${v} more lines`} ${dir}</div><div class="hint">the window grows in place; neighbours stay put</div>`;
    }
    case "conflict": {
      const d = t.h.conflict;
      return [
        `<div class="t bad">conflict · ${d.sides} sides</div>`,
        `<div class="dim">${esc(d.path.split("/").pop()!)} · lines ${d.regions.map((r) => `${r.start}–${r.end}`).join(", ")}</div>`,
        `<div>${d.regions[0]?.sides.map((s) => esc(s.label)).join(" vs ") ?? ""}</div>`,
        `<div class="hint">click to resolve</div>`,
      ].join("");
    }
    case "port": {
      const p = t.p;
      const ends = edges.map((e) => name(c, p.side === "out" ? e.to : e.from));
      return [
        `<div class="t">${esc(p.label)}${p.count > 1 ? ` <span class="dim">×${p.count}</span>` : ""}</div>`,
        `<div class="dim">${p.side === "out" ? "called from" : "called at"} line ${p.line}</div>`,
        ends.length ? `<div>${ends.slice(0, 6).join("<br>")}</div>` : "",
        `<div class="hint">click to fly there</div>`,
      ].join("");
    }
  }
}
