import { dispose } from "./rects";
import { Container, Graphics } from "pixi.js";
import type { ViewBootstrap } from "../contracts/http";
import type { Hub, LinkKind } from "../contracts/model";
import type { Edge } from "../data/client";
import { drawEdges, type LinkGroup } from "./edges";
import type { Theme } from "./theme";

/** Spoke kinds by hub protocol: [producer → hub, hub → consumer]. */
const KINDS: Record<string, [LinkKind, LinkKind]> = {
  bus: ["publish", "subscribe"],
  http: ["http-request", "http-route"],
  env: ["define", "read"],
  config: ["config", "read"],
  deploy: ["deploy", "deploy"],
};

/**
 * A spoke's path: leave (or enter) the window port horizontally, outward from its edge, and meet the
 * hub at its top or bottom vertex vertically. `out`: the port is a right-edge out port.
 */
function spokeCurve(px: number, py: number, out: boolean, hx: number, hy: number, below: boolean, toHub: boolean): Float32Array {
  const dx = Math.abs(hx - px);
  const dy = Math.abs(hy - py);
  const k = Math.min(600, dx / 2 + 120);
  const c1x = px + (out ? k : -k);
  const c2y = hy + (below ? 1 : -1) * Math.max(120, dy / 2);
  const n = 28;
  const pts = new Float32Array(n * 2);
  for (let i = 0; i < n; i++) {
    const t = toHub ? i / (n - 1) : 1 - i / (n - 1);
    const u = 1 - t;
    pts[i * 2] = u * u * u * px + 3 * u * u * t * c1x + 3 * u * t * t * hx + t * t * t * hx;
    pts[i * 2 + 1] = u * u * u * py + 3 * u * u * t * py + 3 * u * t * t * c2y + t * t * t * hy;
  }
  return pts;
}

/** The id a spoke uses for its hub end (never a window id). */
export const hubEnd = (h: Hub) => "hub:" + h.id;

/**
 * Hub spokes: a link from each producer's out port into the hub diamond and from the hub out to
 * each consumer's in port, from the anchors the server places on drawn hubs. They are ordinary
 * `Edge`s, so selection, the Inspector and following work as for code links. Channel hubs draw
 * them faintly at rest (L1+); config hubs only when the hub or one of its windows is selected.
 */
export class Spokes {
  readonly root = new Container();
  byHub = new Map<string, Edge[]>();
  byWindow = new Map<string, Edge[]>();
  private resting: Edge[] = [];
  builtFor = 0;

  setBoot(boot: ViewBootstrap) {
    this.byHub.clear();
    this.byWindow.clear();
    this.resting = [];
    let id = -1;
    const push = (m: Map<string, Edge[]>, k: string, e: Edge) => {
      const l = m.get(k);
      if (l) l.push(e);
      else m.set(k, [e]);
    };
    for (const h of boot.hubs) {
      if (!h.rect || !h.anchors?.length) continue;
      const [kin, kout] = KINDS[h.protocol] ?? ["reference", "reference"];
      const r = h.rect.w / 2;
      const cx = h.rect.x + r;
      const cy = h.rect.y + h.rect.h / 2;
      const sites = [...h.producers.filter((s) => s.window), ...h.consumers.filter((s) => s.window)];
      const nProd = h.producers.filter((s) => s.window).length;
      h.anchors.forEach(([x, y], i) => {
        const s = sites[i];
        if (!s?.window) return;
        const out = i < nProd;
        const site = `${s.path.split("/").pop()}:${s.line}`;
        // Into the vertex facing the window: the bottom one for windows below the hub.
        const below = y > cy;
        const hy = below ? cy + r : cy - r;
        const pts = spokeCurve(x, y, out, cx, hy, below, out);
        const e: Edge = out
          ? { id: id--, kind: kin, from: s.window, to: hubEnd(h), fromCluster: "", toCluster: "", linkId: "", ax: x, ay: y, bx: cx, by: hy, confidence: 1, fromLabel: site, toLabel: h.display, pts }
          : { id: id--, kind: kout, from: hubEnd(h), to: s.window, fromCluster: "", toCluster: "", linkId: "", ax: cx, ay: hy, bx: x, by: y, confidence: 1, fromLabel: h.display, toLabel: site, pts };
        push(this.byHub, h.id, e);
        push(this.byWindow, s.window, e);
        if (h.protocol !== "config") this.resting.push(e);
      });
    }
  }

  /** Spokes drawn per budgeted page (Graphics building costs ~10 µs a spoke). */
  private static PAGE = 300;
  private next?: { c: Container; at: number; theme: Theme; wuPerPx: number; hidden: ReadonlySet<LinkGroup> };

  /**
   * Start rebuilding the at-rest hairlines for channel hubs (dashed and coloured by kind for this
   * tier's scale). `step` builds them page by page inside the frame budget; the previous build stays
   * on screen until the new one is complete (§5: all rebuilds are budgeted).
   */
  start(theme: Theme, wuPerPx: number, hidden: ReadonlySet<LinkGroup>) {
    if (this.next) dispose(this.next.c);
    this.builtFor = wuPerPx;
    this.next = { c: new Container(), at: 0, theme, wuPerPx, hidden };
  }

  get building() {
    return !!this.next;
  }

  /** Build one page; returns it (for tessellation) or undefined when nothing is pending. */
  step(): Graphics | undefined {
    const n = this.next;
    if (!n) return undefined;
    const g = new Graphics();
    const end = Math.min(this.resting.length, n.at + Spokes.PAGE);
    drawEdges(g, this.resting.slice(n.at, end), { theme: n.theme, wuPerPx: n.wuPerPx, detail: false, widthPx: 1, alpha: 0.45, hidden: n.hidden });
    n.c.addChild(g);
    n.at = end;
    if (end >= this.resting.length) {
      for (const ch of this.root.removeChildren()) dispose(ch);
      this.root.addChild(n.c);
      this.next = undefined;
    }
    return g;
  }
}
