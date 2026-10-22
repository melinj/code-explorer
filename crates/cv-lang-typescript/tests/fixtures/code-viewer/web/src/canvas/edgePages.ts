import { Container, Graphics } from "pixi.js";
import type { Edge } from "../data/client";
import { drawEdges, type LinkGroup } from "./edges";
import type { Theme } from "./theme";

const PAGE_EDGES = 1500;
const FLUSH_AT = 600;
const FLUSH_QUIET_MS = 250;
const SMALL_PAGE = 400;

interface Page {
  g: Graphics;
  edges: Edge[];
  stale: boolean;
}

/**
 * Edges longer than a chunk cell. They would make every chunk they leave "visible" from anywhere
 * along their path, so they live in append-only pages instead: a handful of meshes, always drawn,
 * never culled (the GPU clips them). New edges are paged in batches; small pages are merged while
 * the camera is still.
 */
export class EdgePages {
  readonly root = new Container({ isRenderGroup: true, alpha: 0.6 });
  private pages: Page[] = [];
  private pending: Edge[] = [];
  private lastAdd = 0;

  add(edges: Edge[]) {
    this.pending.push(...edges);
    this.lastAdd = performance.now();
  }

  /** Drop everything (view switch). */
  reset() {
    for (const p of this.pages) p.g.destroy();
    this.pages = [];
    this.pending = [];
  }

  /** Every page needs rebuilding (theme change). */
  invalidate() {
    for (const p of this.pages) p.stale = true;
  }

  get count() {
    return this.pages.length;
  }

  /**
   * Do at most one unit of work; returns the Graphics it built, if any. `idle` allows compaction.
   */
  step(theme: Theme, now: number, idle: boolean): Graphics | undefined {
    const stale = this.pages.find((p) => p.stale);
    if (stale) {
      this.draw(stale.g, stale.edges, theme);
      stale.stale = false;
      return stale.g;
    }
    if (this.pending.length >= FLUSH_AT || (this.pending.length && now - this.lastAdd > FLUSH_QUIET_MS)) {
      return this.newPage(this.pending.splice(0, PAGE_EDGES), theme);
    }
    if (idle) {
      const small = this.pages.filter((p) => p.edges.length < SMALL_PAGE);
      if (small.length >= 2) {
        const merged: Edge[] = [];
        for (const p of small) {
          if (merged.length + p.edges.length > PAGE_EDGES) break;
          merged.push(...p.edges);
          this.pages.splice(this.pages.indexOf(p), 1);
          p.g.destroy();
        }
        return this.newPage(merged, theme);
      }
    }
    return undefined;
  }

  hidden: ReadonlySet<LinkGroup> = new Set();

  private draw(g: Graphics, edges: Edge[], theme: Theme) {
    g.clear();
    drawEdges(g, edges, { theme, wuPerPx: 1, detail: false, widthPx: 1, alpha: 0.8, hidden: this.hidden });
  }

  private newPage(edges: Edge[], theme: Theme) {
    const g = new Graphics();
    this.draw(g, edges, theme);
    this.pages.push({ g, edges, stale: false });
    this.root.addChild(g);
    return g;
  }
}
