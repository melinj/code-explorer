import { Container, Graphics } from "pixi.js";
import type { Cluster, Hub, Rect, WindowHeader } from "../contracts/model";
import type { Edge } from "../data/client";
import type { Highway } from "./bundles";
import type { PortHit } from "./chunks";
import type { ConflictHit, ThreadHit } from "./threadMarks";
import type { ExpandHit } from "../windows/codeText";
import { drawEdges, quadEdges, type LinkGroup } from "./edges";
import { RectBatch, dispose } from "./rects";
import type { Theme } from "./theme";

/** Something on the canvas the pointer can hover or select. */
export type Target =
  | { type: "window"; w: WindowHeader }
  | { type: "cluster"; c: Cluster }
  | { type: "edge"; e: Edge }
  | { type: "highway"; h: Highway }
  | { type: "hub"; h: Hub }
  | { type: "port"; p: PortHit }
  | { type: "thread"; h: ThreadHit }
  | { type: "conflict"; h: ConflictHit }
  | { type: "expand"; h: ExpandHit };

export function targetKey(t?: Target): string {
  if (!t) return "";
  switch (t.type) {
    case "window":
      return "w:" + t.w.id;
    case "cluster":
      return "c:" + t.c.id;
    case "edge":
      return "e:" + t.e.id;
    case "highway":
      return "h:" + t.h.key;
    case "hub":
      return "u:" + t.h.id;
    case "port":
      return `p:${t.p.window.id}:${t.p.side}:${t.p.line}`;
    case "thread":
      return "t:" + t.h.thread.id;
    case "conflict":
      return "x:" + t.h.conflict.path;
    case "expand":
      return `m:${t.h.window.id}:${JSON.stringify(t.h.req)}`;
  }
}

/** Above this many links the overlay draws hairlines only, to stay cheap. */
const DETAIL_LIMIT = 1200;

export interface OverlayInput {
  theme: Theme;
  zoom: number;
  hidden: ReadonlySet<LinkGroup>;
  selected?: Target;
  hover?: Target;
  /** Individual links behind a target (unbundled). */
  edgesOf: (t: Target) => Edge[];
  windowOf: (id: string) => WindowHeader | undefined;
  /** A cluster's highways, busiest first: what hovering or selecting a cluster lights up. */
  highwaysOf?: (clusterId: string) => Highway[];
  /** Windows in and around the view; those not linked to the selection are dimmed. */
  windowsNear: () => WindowHeader[];
  /** A selected split file (D14): its fragments stay bright, everything else dims. */
  selectedPath?: string;
  /** L0: windows are not drawn, so focus dims whole clusters (those in and around the view). */
  galaxy?: () => Cluster[];
  /**
   * Hover shows the target's outline only, not its static links or highways: the co-change lens
   * draws what changes with it instead (a selection still unbundles in full).
   */
  quietHover?: boolean;
  /**
   * Depends lens: a cluster's highways in its colours, those coming in (it is depended on) and
   * those going out (it depends on them).
   */
  flowColors?: { into: number; out: number };
}

/**
 * Focus layer above the windows: the selected (and hovered) target's links unbundled and drawn in
 * full, its outline, and outlines on the windows at the other ends. Rebuilt only when the targets
 * change or the zoom moves far from the zoom it was sized for.
 */
export class Overlay {
  /** Focused links: beneath the windows (they never cross code) but above every dimmed link. */
  readonly links = new Container();
  /** Outlines of the focused targets, above the windows. */
  readonly root = new Container();
  private lg = new Container();
  private g = new Graphics();
  builtZoom = 0;
  private dimLayer?: Container;

  constructor() {
    this.links.addChild(this.lg);
    this.root.addChild(this.g);
  }

  /** Returns true when something is highlighted (the engine dims the rest). */
  draw(o: OverlayInput): boolean {
    const g = this.g.clear();
    for (const ch of this.lg.removeChildren()) dispose(ch);
    const lines = new Graphics();
    const rb = new RectBatch(256);
    this.builtZoom = o.zoom;
    const px = 1 / o.zoom;
    const t = o.theme;
    const layers: Array<[Target | undefined, number, number]> = [
      [o.hover && o.hover !== o.selected ? o.hover : undefined, 1.8, 0.85],
      [o.selected, 2.4, 1],
    ];
    let active = false;
    for (const [target, width, alpha] of layers) {
      if (!target) continue;
      active = true;
      const quiet = !!o.quietHover && target !== o.selected;
      const edges = quiet ? [] : o.edgesOf(target);
      const detail = edges.length <= DETAIL_LIMIT;
      const opts = { theme: t, wuPerPx: px, detail, widthPx: width, alpha, hidden: o.hidden };
      if (detail) quadEdges(rb, edges, opts);
      else drawEdges(lines, edges, opts);
      // The windows at the other ends of those links.
      const seen = new Set<string>();
      for (const e of edges.slice(0, 400)) {
        for (const id of [e.from, e.to]) {
          if (seen.has(id) || (target.type === "window" && id === target.w.id)) continue;
          seen.add(id);
          const w = o.windowOf(id);
          if (w) outline(g, w.rect, 1.5 * px, t.ink, 0.55 * alpha);
        }
      }
      const color = target === o.selected ? t.accent : t.ink;
      const bw = (target === o.selected ? 3 : 2) * px;
      switch (target.type) {
        case "window":
          outline(g, target.w.rect, bw, color, 1, 3 * px);
          break;
        case "cluster": {
          // Quiet hover: the co-change lens rings the cluster itself.
          if (!quiet) outline(g, target.c.rect, bw, color, 1, 30);
          // Its highways (at L0 the only links there are), with the links beneath the windows, and
          // the clusters at their other ends.
          for (const h of quiet ? [] : (o.highwaysOf?.(target.c.id).slice(0, 80) ?? [])) {
            const p = h.pts;
            lines.moveTo(p[0], p[1]);
            for (let i = 2; i < p.length; i += 2) lines.lineTo(p[i], p[i + 1]);
            const c = o.flowColors ? (h.to.id === target.c.id ? o.flowColors.into : o.flowColors.out) : color;
            lines.stroke({ width: (1.5 + Math.min(4, Math.log2(1 + h.count))) * px, color: c, alpha: (o.flowColors ? 0.8 : 0.55) * alpha, cap: "round" });
            const other = h.from.id === target.c.id ? h.to : h.from;
            outline(g, other.rect, 1.5 * px, color, 0.6 * alpha, 30);
          }
          break;
        }
        case "highway": {
          // With the links, beneath the windows and clusters it runs past.
          const p = target.h.pts;
          lines.moveTo(p[0], p[1]);
          for (let i = 2; i < p.length; i += 2) lines.lineTo(p[i], p[i + 1]);
          lines.stroke({ width: 4 * px, color, alpha: 0.35, cap: "round" });
          outline(g, target.h.from.rect, bw, color, 0.8, 30);
          outline(g, target.h.to.rect, bw, color, 0.8, 30);
          break;
        }
        case "edge":
          quadEdges(rb, [target.e], { theme: t, wuPerPx: px, detail: true, widthPx: 3, alpha: 1, hidden: new Set() });
          break;
        case "hub":
          if (target.h.rect) outline(g, target.h.rect, bw, color, 1, 10);
          break;
        case "port":
          outline(g, target.p, 1.5 * px, color, 1, 2 * px);
          break;
        case "thread":
        case "conflict":
        case "expand":
          outline(g, target.h, 1.5 * px, color, 1, 2 * px);
          break;
      }
    }
    const quads = new Container();
    rb.build(quads);
    this.lg.addChild(lines, quads);
    // Selection focus: windows not linked to the selection fade back (outlines stay above).
    const sel = o.selected;
    if (sel && sel.type !== "hub") {
      const keep = new Set<string>();
      for (const e of o.edgesOf(sel)) keep.add(e.from).add(e.to);
      if (sel.type === "window") keep.add(sel.w.id);
      const file = sel.type === "window" && sel.w.fragment && o.selectedPath === sel.w.path ? sel.w.path : undefined;
      if (sel.type === "port") keep.add(sel.p.window.id);
      const clusters = sel.type === "cluster" ? new Set([sel.c.id]) : sel.type === "highway" ? new Set([sel.h.from.id, sel.h.to.id]) : undefined;
      const dim = new RectBatch(512);
      if (o.galaxy) {
        const lit = new Set(clusters);
        for (const id of keep) {
          const w = o.windowOf(id);
          if (w) lit.add(w.cluster);
        }
        for (const c of o.galaxy()) if (!lit.has(c.id)) dim.round(c.rect.x - 2 * px, c.rect.y - 2 * px, c.rect.w + 4 * px, c.rect.h + 4 * px, 200, t.bg, 0.62);
      } else {
        for (const w of o.windowsNear()) {
          if (keep.has(w.id) || clusters?.has(w.cluster) || (file && w.path === file)) continue;
          dim.add(w.rect.x - 2 * px, w.rect.y - 2 * px, w.rect.w + 4 * px, w.rect.h + 4 * px, t.bg, 0.62);
        }
      }
      const dc = new Container();
      dim.build(dc);
      this.root.addChildAt(dc, 0);
      this.dimLayer && dispose(this.dimLayer);
      this.dimLayer = dc;
    } else {
      this.dimLayer && dispose(this.dimLayer);
      this.dimLayer = undefined;
    }
    return active;
  }
}

function outline(g: Graphics, r: Rect, w: number, color: number, alpha: number, gap = 0) {
  g.rect(r.x - gap, r.y - gap, r.w + 2 * gap, r.h + 2 * gap).stroke({ width: w, color, alpha, alignment: 1 });
}
