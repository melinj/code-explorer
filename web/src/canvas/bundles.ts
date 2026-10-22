import { Container } from "pixi.js";
import type { ViewBootstrap } from "../contracts/http";
import type { Cluster, EdgeBundle, LinkKind, Rect } from "../contracts/model";
import { GROUP_OF, curve, edgeColor, taperedPolyline, type LinkGroup } from "./edges";
import { RectBatch, dispose } from "./rects";
import { mix, type Theme } from "./theme";

const PAGE = 250;
/** Highway contrast by link count (share mixed toward the background), weakest first. */
const REST = [0.84, 0.76, 0.68, 0.6, 0.52, 0.44];

export interface Highway {
  key: string; // "from>to", as DataClient.edgesByPair
  bundle: EdgeBundle;
  from: Cluster;
  to: Cluster;
  pts: Float32Array;
  box: Rect;
  /** Visible link count after filters, and the kind that dominates it. */
  count: number;
  kind: LinkKind;
}

/**
 * Cross-cluster links drawn as one "highway" per cluster pair (from the T0 bootstrap), width by link
 * count and colour by the dominant kind. Selecting a window, cluster or highway unbundles it into
 * individual links (the engine's overlay). Rebuilt in pages when the tier or the link filters change;
 * the previous build stays on screen until the new one is complete.
 */
export class Bundles {
  readonly root = new Container({ isRenderGroup: true });
  highways: Highway[] = [];
  /** Per highway (same order): its link kinds and counts. */
  private kinds: Array<Array<[LinkKind, number]>> = [];
  private current = new Container();
  private next?: {
    c: Container;
    at: number;
    wuPerPx: number;
    theme: Theme;
    hidden: ReadonlySet<LinkGroup>;
    minCount: number;
    dim: boolean;
    fades?: (c: Cluster) => boolean;
    cased: boolean;
    /** Highway indexes, weakest first, so the busiest draw on top. */
    order: Uint32Array;
  };
  /** Widest highway of the last build, in screen px, and highways a lens stepped back (e2e checks). */
  maxWidthPx = 0;
  fadedCount = 0;
  private building_max = 0;
  private building_faded = 0;

  constructor() {
    this.root.addChild(this.current);
  }

  setData(boot: ViewBootstrap) {
    const byId = new Map(boot.clusters.map((c) => [c.id, c]));
    this.highways = [];
    this.kinds = [];
    this.ranked = undefined;
    // Leave and enter through the facing sides of the two cluster boxes. Highways sharing a side
    // fan out along it, ordered by where their other end is, so they don't all leave from one point
    // and cross right away.
    const ends: Array<{ b: EdgeBundle; from: Cluster; to: Cluster; fwd: boolean; ay: number; by: number }> = [];
    const sides = new Map<string, { c: Cluster; list: Array<{ end: (typeof ends)[number]; a: boolean; toward: number }> }>();
    const side = (c: Cluster, right: boolean, end: (typeof ends)[number], a: boolean, toward: number) => {
      const k = c.id + (right ? ">" : "<");
      let s = sides.get(k);
      if (!s) sides.set(k, (s = { c, list: [] }));
      s.list.push({ end, a, toward });
    };
    for (const b of boot.bundles) {
      const from = byId.get(b.from);
      const to = byId.get(b.to);
      if (!from || !to) continue;
      const fwd = to.rect.x + to.rect.w / 2 >= from.rect.x + from.rect.w / 2;
      const end = { b, from, to, fwd, ay: 0, by: 0 };
      ends.push(end);
      side(from, fwd, end, true, to.rect.y + to.rect.h / 2);
      side(to, !fwd, end, false, from.rect.y + from.rect.h / 2);
    }
    for (const { c, list } of sides.values()) {
      const r = c.rect;
      list.sort((p, q) => p.toward - q.toward);
      // Spread over the middle of the side, never closer than the box's corner radius to its ends.
      const span = Math.max(0, r.h - 400) * 0.8;
      list.forEach((p, i) => {
        const y = r.y + r.h / 2 + (list.length > 1 ? (i / (list.length - 1) - 0.5) * span : 0);
        if (p.a) p.end.ay = y;
        else p.end.by = y;
      });
    }
    for (const { b, from, to, fwd, ay, by } of ends) {
      const ax = fwd ? from.rect.x + from.rect.w : from.rect.x;
      const bx = fwd ? to.rect.x : to.rect.x + to.rect.w;
      // A leftward highway is the rightward curve between the same sides, traversed backwards.
      const pts = fwd ? curve(ax, ay, bx, by, 24) : reversePairs(curve(bx, by, ax, ay, 24));
      let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
      for (let i = 0; i < pts.length; i += 2) {
        x0 = Math.min(x0, pts[i]);
        x1 = Math.max(x1, pts[i]);
        y0 = Math.min(y0, pts[i + 1]);
        y1 = Math.max(y1, pts[i + 1]);
      }
      this.highways.push({ key: b.from + ">" + b.to, bundle: b, from, to, pts, box: { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }, count: 0, kind: "call" });
      // Link kinds as a flat list once, so restyles (every tier change) allocate nothing per highway.
      this.kinds.push(Object.entries(b.kinds) as Array<[LinkKind, number]>);
    }
  }

  /**
   * Start a rebuild for this zoom and these filters; `step` does the work in pages.
   * - `minCount`: highways with fewer links are left out (zoomed out, single links are noise).
   * - `maxShown`: at most this many, the busiest.
   * - `dim` (something is in focus): every highway steps back toward the background.
   * - `fades`: clusters the lens leaves out; their highways step back with them.
   * - `cased`: each highway over a background-coloured casing, so crossings read as one over the
   *   other (zoomed out, where highways are the only links; zoomed in they are context only).
   */
  restyle(theme: Theme, wuPerPx: number, hidden: ReadonlySet<LinkGroup>, minCount = 1, maxShown = Infinity, dim = false, fades?: (c: Cluster) => boolean, cased = true) {
    if (this.next) dispose(this.next.c);
    const { sorted, order } = this.rank(hidden);
    // Zoomed out, only the strongest highways read as structure (thousands draw as a grey mat):
    // raise the threshold so at most `maxShown` are drawn. Selecting a cluster still unbundles all.
    if (maxShown < this.highways.length) {
      minCount = Math.max(minCount, sorted[maxShown - 1] + 1, 1);
      // Ties at the cut would overshoot; keep the cut but never drop below the top few.
      let above = 0;
      for (const c of sorted) if (c >= minCount) above++;
      if (above < Math.min(40, maxShown)) minCount = Math.max(1, sorted[Math.min(40, maxShown) - 1]);
    }
    this.next = { c: new Container(), at: 0, wuPerPx, theme, hidden, minCount, dim, fades, cased, order };
    this.building_max = 0;
    this.building_faded = 0;
  }

  /**
   * Link counts under these filters, busiest first, and the highways weakest first (the draw order:
   * the busiest on top). Kept per filter set: restyles on every tier change reuse them.
   */
  private ranked?: { key: string; sorted: Float64Array; order: Uint32Array };
  private rank(hidden: ReadonlySet<LinkGroup>) {
    const key = [...hidden].sort().join(",");
    if (this.ranked?.key === key && this.ranked.order.length === this.highways.length) return this.ranked;
    const counts = new Float64Array(this.highways.length);
    for (let i = 0; i < counts.length; i++) {
      let n = 0;
      for (const [k, c] of this.kinds[i]) if (!hidden.has(GROUP_OF[k])) n += c;
      counts[i] = n;
    }
    const order = new Uint32Array(counts.length);
    for (let i = 0; i < order.length; i++) order[i] = i;
    order.sort((a, b) => counts[a] - counts[b]);
    this.ranked = { key, sorted: counts.slice().sort().reverse(), order };
    return this.ranked;
  }

  get building() {
    return !!this.next;
  }

  /**
   * Build one page of highways as quads; returns true while work remains. A page ends early once
   * `deadline` (performance.now()) passes, so one step never runs far past the frame budget.
   */
  step(deadline = Infinity): boolean {
    const n = this.next;
    if (!n) return false;
    const rb = new RectBatch(PAGE * (n.cased ? 48 : 24));
    let end = Math.min(this.highways.length, n.at + PAGE);
    const t = n.theme;
    for (let j = n.at; j < end; j++) {
      if (((j - n.at) & 15) === 15 && performance.now() > deadline) {
        end = j;
        break;
      }
      const i = n.order[j];
      const h = this.highways[i];
      let count = 0;
      let best = 0;
      for (const [k, c] of this.kinds[i]) {
        if (n.hidden.has(GROUP_OF[k])) continue;
        count += c;
        if (c > best) {
          best = c;
          h.kind = k;
        }
      }
      h.count = count;
      if (!count || count < n.minCount) continue;
      // Width grows with log2 of the link count: 1–2 links thin, busy highways up to 9 px. Contrast
      // grows with it too, so the few busy highways read as structure and the many thin ones as a
      // faint texture (all at one strength they were a grey mat).
      const w = Math.min(7, 1.1 + 1.2 * Math.log2(count));
      if (w > this.building_max) this.building_max = w;
      // Six contrast steps (a colour each per kind: the rect palette holds few, rects.ts).
      let rest = REST[Math.min(REST.length - 1, Math.floor(Math.log2(count) / 1.2))];
      const faded = n.fades ? (n.fades(h.from) ? 1 : 0) + (n.fades(h.to) ? 1 : 0) : 0;
      if (faded) {
        rest = Math.max(rest, faded === 2 ? 0.93 : 0.86);
        this.building_faded++;
      }
      const toBg = n.dim ? rest + (1 - rest) * 0.75 : rest;
      const px = n.wuPerPx;
      // Opaque, pre-mixed toward the background: translucent quads (or a translucent layer) would
      // double up at the joints and read as dashes. Tapered from the calling cluster to the
      // called one, over a casing in the background colour.
      if (n.cased) rb.polyline(h.pts, (w * 1.3 + 2.5) * px, t.bg);
      // Calls (most highways) lean toward the accent, so they read as links rather than grey fills.
      const kc = GROUP_OF[h.kind] === "calls" ? mix(edgeColor(t, h.kind), t.accent, 0.22) : edgeColor(t, h.kind);
      taperedPolyline(rb, h.pts, w * 1.3 * px, w * 0.7 * px, mix(kc, t.bg, toBg));
    }
    const page = new Container();
    rb.build(page);
    n.c.addChild(page);
    n.at = end;
    if (end >= this.highways.length) {
      this.root.removeChild(this.current);
      dispose(this.current);
      this.current = n.c;
      this.root.addChild(n.c);
      this.next = undefined;
      this.maxWidthPx = this.building_max;
      this.fadedCount = this.building_faded;
    }
    return true;
  }
}

function reversePairs(p: Float32Array): Float32Array {
  const out = new Float32Array(p.length);
  for (let i = 0, j = p.length - 2; i < p.length; i += 2, j -= 2) {
    out[i] = p[j];
    out[i + 1] = p[j + 1];
  }
  return out;
}
