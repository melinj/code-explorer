import type { BlobText, ViewBootstrap, WindowBatchResponse } from "../contracts/http";
import type { Link, LinkKind, Rect, ViewRef, WindowDiff, WindowHeader, WindowOutline, ZoomTier } from "../contracts/model";
import { FILE_CHIP_SLOTS, TILE_BASE_WU } from "../contracts/wire";
import { EDGE_STRIDE, LINK_KINDS, type DecodedOutline, type PackedTile } from "./fetch.worker";
import { Loader, grow, intersects } from "./loader";

/** Spatial cell size for the client index and the canvas chunks. */
export const CELL_WU = 16384;

export type Outline = Omit<WindowOutline, "silhouette"> & { silhouette: Uint8Array };

/** A drawable link (from TileEdge): out port a, in port b, in world units. `id` is interned. */
export interface Edge {
  id: number;
  kind: LinkKind;
  from: string;
  to: string;
  fromCluster: string;
  toCluster: string;
  /** The server's link id (Link.id), to look up labels and sites from outline batches. */
  linkId: string;
  /** Flattened curve (x, y pairs), computed on first use for drawing and hit testing. */
  pts?: Float32Array;
  ax: number;
  ay: number;
  bx: number;
  by: number;
  confidence: number;
  /** Names of the ends when they aren't windows (hub spokes). */
  fromLabel?: string;
  toLabel?: string;
}

/** Cells dropped per eviction pass at most. */
const EVICT_CELLS_PER_PASS = 8;

/** Proximity grid for file-chip collisions: about a screen at outline zoom. */
export const CHIP_CELL_WU = 4096;

/** Edges spanning more than this go to the global long-edge layer instead of a chunk. */
export const LONG_EDGE_WU = CELL_WU;

export interface ClientEvents {
  windows(cells: Set<string>): void;
  edges(cells: Set<string>): void;
  longEdges(edges: Edge[]): void;
  /** Any edges arrived (including cross-cluster ones, which only the overlay draws). */
  edgesLoaded(): void;
  outlines(cells: Set<string>): void;
  text(windowIds: string[]): void;
  /** Long edges were pruned with evicted cells: the pages are rebuilt from these. */
  longEdgesReset(edges: Edge[]): void;
  /** Cells whose windows and short edges were dropped to keep memory bounded (far from the camera). */
  evicted(cells: Set<string>, windowIds: string[]): void;
  /**
   * The server answered 404 "view-unknown" for the view on screen (it restarted since the view was
   * opened, or dropped it): re-POST the spec (http.ts "Views after a restart").
   */
  viewLost(): void;
}

/**
 * True for an answer meaning "this view id is not loaded" (404 `view-unknown`, http.ts): `body` is
 * the parsed JSON body or its text (the fetch worker reports errors as text).
 */
export function isViewUnknown(status: number, body: unknown): boolean {
  if (status !== 404) return false;
  let b = body;
  if (typeof b === "string") {
    try {
      b = JSON.parse(b);
    } catch {
      return false;
    }
  }
  const o = (b ?? {}) as { code?: string; error?: { code?: string } };
  return (o.error?.code ?? o.code) === "view-unknown";
}

export function cellKey(cx: number, cy: number) {
  return cx + "," + cy;
}

/** `r` cut to `bounds` (an empty rect when they do not meet; `r` itself without bounds). */
export function clampTo(r: Rect, bounds?: Rect): Rect {
  if (!bounds) return r;
  const x = Math.max(r.x, bounds.x);
  const y = Math.max(r.y, bounds.y);
  const w = Math.min(r.x + r.w, bounds.x + bounds.w) - x;
  const h = Math.min(r.y + r.h, bounds.y + bounds.h) - y;
  return w >= 0 && h >= 0 ? { x, y, w, h } : { x, y, w: -1, h: -1 };
}

export function cellsOf(r: Rect): Array<[number, number]> {
  const out: Array<[number, number]> = [];
  if (!(r.w >= 0 && r.h >= 0)) return out; // empty (clampTo) or NaN
  const x0 = Math.floor(r.x / CELL_WU);
  const x1 = Math.floor((r.x + r.w) / CELL_WU);
  const y0 = Math.floor(r.y / CELL_WU);
  const y1 = Math.floor((r.y + r.h) / CELL_WU);
  for (let x = x0; x <= x1; x++) for (let y = y0; y <= y1; y++) out.push([x, y]);
  return out;
}

class Lru<V> {
  private map = new Map<string, V>();
  /** `onEvict` hears about entries pushed out by the cap (not explicit deletes). */
  constructor(
    private cap: number,
    private onEvict?: (k: string) => void,
  ) {}
  get(k: string) {
    const v = this.map.get(k);
    if (v !== undefined) {
      this.map.delete(k);
      this.map.set(k, v);
    }
    return v;
  }
  has(k: string) {
    return this.map.has(k);
  }
  delete(k: string) {
    this.map.delete(k);
  }
  set(k: string, v: V) {
    this.map.delete(k);
    this.map.set(k, v);
    while (this.map.size > this.cap) {
      const k = this.map.keys().next().value!;
      this.map.delete(k);
      this.onEvict?.(k);
    }
  }
}

/**
 * F2 data client: T0 bootstrap, T1 tiles, T2 outline batches, T3 blob text. Windows are indexed by
 * the cell that holds their top-left corner; edges by the cell of each port.
 */
export class DataClient {
  readonly loader = new Loader();
  view!: ViewRef;
  boot!: ViewBootstrap;
  windows = new Map<string, WindowHeader>();
  windowsByPath = new Map<string, WindowHeader[]>();
  cells = new Map<string, WindowHeader[]>();
  /** Every loaded edge by id (an index list entry is live only while it is the object here). */
  private edgeIds = new Map<number, Edge>();
  /** Intra-cluster edges by the cell of their out port; drawn individually. */
  edgeCells = new Map<string, Edge[]>();
  longEdges: Edge[] = [];
  /** Every loaded edge by window, by cluster and by cluster pair ("a>b"), for unbundling. */
  edgesByWindow = new Map<string, Edge[]>();
  private edgesByCluster = new Map<string, Edge[]>();
  private edgesByPair = new Map<string, Edge[]>();
  /**
   * Cluster and pair lists that eviction left holding dead edges. They are long (a busy cluster has
   * thousands of links), so eviction never filters them: they are compacted in place when read, or
   * by `sweep` in idle frames.
   */
  private staleLists = new Set<string>();

  private live = (e: Edge) => this.edgeIds.get(e.id) === e;

  private fresh(m: Map<string, Edge[]>, tag: string, key: string): Edge[] {
    const list = m.get(key);
    if (!list) return [];
    if (this.staleLists.delete(tag + key)) {
      compact(list, this.live);
      if (!list.length) m.delete(key);
    }
    return list;
  }

  /** Loaded edges touching a cluster. */
  clusterEdges(id: string): Edge[] {
    return this.fresh(this.edgesByCluster, "c", id);
  }

  /** Loaded edges of one highway ("from>to"). */
  pairEdges(key: string): Edge[] {
    return this.fresh(this.edgesByPair, "p", key);
  }

  /** Compact stale lists for up to `budgetMs` (idle frames). */
  sweep(budgetMs: number) {
    this.pruneLinks();
    if (!this.staleLists.size) return;
    const t0 = performance.now();
    for (const tag of this.staleLists) {
      if (tag[0] === "c") this.fresh(this.edgesByCluster, "c", tag.slice(1));
      else this.fresh(this.edgesByPair, "p", tag.slice(1));
      if (performance.now() - t0 > budgetMs) break;
    }
  }
  /**
   * Outlines (silhouettes, ports) are drawn from L2 on, for windows around the view: a few thousand
   * cover any L2 view with room for coming back. The links of outline batches go with them (below).
   */
  outlines = this.newOutlines();
  texts = new Lru<BlobText>(200);
  /** Outlines the cap pushed out since `links` was last pruned. */
  private outlinesDropped = 0;
  private newOutlines() {
    return new Lru<Outline>(3000, () => this.outlinesDropped++);
  }

  /**
   * Links arrive with outline batches and serve the outlines' ports (labels, dynamic stubs) and the
   * inspector. Drop those neither of whose windows has an outline any more: without this they
   * outlived their outlines and grew with every window ever seen at L2 (pruned only when both
   * windows were evicted). Cheap (one pass over the map); run when enough outlines have gone.
   */
  pruneLinks(force = false) {
    if (!force && this.outlinesDropped < 256) return 0;
    this.outlinesDropped = 0;
    let n = 0;
    for (const l of this.links.values()) {
      const from = l.from.window;
      const to = "window" in l.to ? l.to.window : undefined;
      if ((from && this.outlines.has(from)) || (to && this.outlines.has(to))) continue;
      this.links.delete(l.id);
      n++;
    }
    return n;
  }
  private tilesDone = new Set<string>();
  private outlinePending = new Set<string>();
  private textPending = new Set<string>();
  private listeners: Partial<ClientEvents> = {};
  private base: string;

  constructor(base = "/api/v1") {
    this.base = base;
  }

  on(ev: Partial<ClientEvents>) {
    Object.assign(this.listeners, ev);
  }

  /**
   * Report a failed view-scoped request (a loader error `{status, message}` or a fetch Response):
   * a 404 "view-unknown" for the current view tells the listeners (viewLost), once per view id.
   * Never throws; other failures are ignored, as before.
   */
  async noteFailure(err: unknown, viewId = this.view?.id) {
    let status = 0;
    let body: unknown;
    if (typeof Response !== "undefined" && err instanceof Response) {
      status = err.status;
      body = await err.clone().text().catch(() => "");
    } else if (err && typeof err === "object" && "status" in err) {
      status = (err as { status: number }).status;
      body = (err as { message?: string }).message;
    }
    if (!isViewUnknown(status, body) || !viewId || viewId !== this.view?.id || this.lostView === viewId) return;
    this.lostView = viewId;
    this.listeners.viewLost?.();
  }
  private lostView?: string;

  /** Bumped on every view switch; responses for an older view are dropped. */
  private gen = 0;
  /** Links from outline batches by id: labels and both sites, even when an end isn't loaded. */
  links = new Map<string, Link>();
  /** Range views: diff lines per hunk window (T2 "diff" part). */
  diffs = new Map<string, WindowDiff>();
  /**
   * Loaded windows kept at most (§5: memory stays bounded). Past it, cells far from the camera are
   * dropped with their short edges, outlines and diffs, and reload like any other tile when the
   * camera comes back.
   */
  windowCap = 16000;
  private lastEvict = 0;
  /** File-chip slot per split file (D14): the server's slot, shifted when a nearby split file has it. */
  private chipSlots = new Map<string, number>();
  private chipByCell = new Map<string, Map<string, number>>();
  /** Range views: owner (change) → stripe slot, in order of first appearance (stable per view). */
  private owners = new Map<string, number>();

  get isRange() {
    return this.view?.spec.mode === "range";
  }

  /**
   * Switch to another view (a range, or back to the snapshot). Everything spatial is dropped and
   * re-fetched; blob text stays cached (it is content-addressed).
   */
  async switchView(spec: unknown, current: () => boolean = () => true) {
    // Create the view first: a refused spec (bad revset, not a chain) leaves the current view intact.
    const view = await this.createView(spec);
    // A newer switch started meanwhile (e.g. back to the snapshot during a stale range's reload): it wins.
    if (!current()) return undefined;
    this.gen++;
    this.loader.tell({ type: "reset" });
    // No requests until the new view is open: the old view id would still be used.
    this.switching = true;
    this.loader.cancelAll();
    this.windows.clear();
    this.windowsByPath.clear();
    this.cells.clear();
    this.edgeIds.clear();
    this.longSet.clear();
    this.edgeCells.clear();
    this.longEdges = [];
    this.edgesByWindow.clear();
    this.edgesByCluster.clear();
    this.edgesByPair.clear();
    this.staleLists.clear();
    this.outlines = this.newOutlines();
    this.outlinesDropped = 0;
    this.diffs.clear();
    this.links.clear();
    this.owners.clear();
    this.cellSeen.clear();
    this.evictedAt.clear();
    this.tilesDone.clear();
    this.outlinePending.clear();
    try {
      this.view = view;
      this.lostView = undefined;
      this.boot = await (await fetch(`${this.base}/views/${view.id}/bootstrap`)).json();
      return this.boot;
    } finally {
      this.switching = false;
    }
  }
  private switching = false;

  /** POST /views; throws with the server's message (and hint) when it refuses the spec. */
  private async createView(spec: unknown): Promise<ViewRef> {
    const r = await fetch(`${this.base}/views`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(spec),
    });
    if (!r.ok) {
      const b = await r.json().catch(() => ({}));
      const e = b.error ?? b;
      const hint = e.details?.hint ?? e.hint;
      throw Object.assign(new Error(`${e.message ?? r.statusText}${hint ? ` (${hint})` : ""}`), { code: e.code ?? String(r.status) });
    }
    return r.json();
  }

  async open(spec: unknown = { mode: "snapshot", rev: "@" }) {
    this.view = await this.createView(spec);
    this.boot = await (await fetch(`${this.base}/views/${this.view.id}/bootstrap`)).json();
    return this.boot;
  }

  /** Called by the engine when the camera settles or moves; cheap when nothing is new. */
  update(view: Rect, zoom: number, tier: ZoomTier, velocity: [number, number]) {
    if (this.switching) return;
    this.lastView = view;
    // Predict ~250 ms ahead, keep one screen of margin around it.
    const predicted = { ...view, x: view.x - velocity[0] * 0.25 / zoom, y: view.y - velocity[1] * 0.25 / zoom };
    const keep = grow(predicted, view.w * 1.5, view.h * 1.5);
    this.loader.setFocus(predicted, keep);
    // LRU: cells the camera has been near recently are the last to go.
    const now = performance.now();
    // Only cells inside the world can hold data: clamped, this map stays bounded by the world's cells
    // (unclamped, a far zoomed-out camera put ~10^7 empty off-world cells in it per update: "Map
    // maximum size exceeded", and a frozen main thread).
    for (const [x, y] of cellsOf(clampTo(keep, this.boot?.world))) this.cellSeen.set(cellKey(x, y), now);
    // The caps are hard (§5): over them, evict now; near them, a few cells per pass.
    if (this.overCap()) this.evict(view, true);
    else if ((this.windows.size > this.windowCap * 0.9 || this.edgeIds.size > this.edgeCap * 0.9) && now - this.lastEvict > 400) this.evict(view);
    // L0 draws from the bootstrap only; a range view's windows are few, so it loads them at any zoom.
    if (zoom < 0.02 && !this.isRange) return;
    // Near the window cap, prefetch nothing beyond the view: what is fetched would only push out
    // what was just loaded.
    const near = this.windows.size >= this.windowCap * 0.9;
    this.requestTiles(near ? view : grow(predicted, view.w * 0.5, view.h * 0.5), zoom);
    if (tier === "L2" || tier === "L3") this.requestOutlines(grow(view, view.w * 0.25, view.h * 0.25));
    if (tier === "L3") this.requestTexts(grow(view, view.w * 0.1, view.h * 0.1));
  }

  private overCap() {
    return this.windows.size > this.windowCap || this.edgeIds.size > this.edgeCap;
  }

  /** The view the camera last reported (eviction around it when tiles arrive between updates). */
  private lastView?: Rect;
  /** Most windows held at once after an arrival was processed (diagnostics: stays ≤ windowCap). */
  peakWindows = 0;

  /** Edges kept at most (lexical-candidate links make dense repos carry several per window). */
  edgeCap = 60000;
  /** Last time each cell was near the camera (LRU for eviction). */
  private cellSeen = new Map<string, number>();

  /**
   * Drop the least recently seen cells (then the farthest from `view`) until the caps have room.
   * Soft passes (near the caps) drop a few cells, keep everything within a screen of the view and
   * aim at 75% of the caps; hard passes (over a cap) drop as many cells as it takes to get under
   * 90%, sparing only the view itself, so the caps hold strictly. Tiles over the dropped cells are
   * forgotten so they load again on return; long edges stay unless both ends go.
   */
  evict(view: Rect, hard = false) {
    this.lastEvict = performance.now();
    const keep = hard ? grow(view, view.w * 0.1, view.h * 0.1) : grow(view, view.w, view.h);
    const cx = view.x + view.w / 2;
    const cy = view.y + view.h / 2;
    // Least recently seen first; among cells seen at the same time, the farthest.
    const cands: Array<{ k: string; seen: number; d: number }> = [];
    const consider = (k: string) => {
      const i = k.indexOf(",");
      const x = +k.slice(0, i) * CELL_WU;
      const y = +k.slice(i + 1) * CELL_WU;
      if (x < keep.x + keep.w && keep.x < x + CELL_WU && y < keep.y + keep.h && keep.y < y + CELL_WU) return;
      cands.push({ k, seen: this.cellSeen.get(k) ?? 0, d: Math.hypot(x + CELL_WU / 2 - cx, y + CELL_WU / 2 - cy) });
    };
    for (const k of this.cells.keys()) consider(k);
    for (const k of this.edgeCells.keys()) if (!this.cells.has(k)) consider(k);
    cands.sort((a, b) => a.seen - b.seen || b.d - a.d);
    const target = this.windowCap * (hard ? 0.9 : 0.75);
    const edgeTarget = this.edgeCap * (hard ? 0.9 : 0.75);
    const maxCells = hard ? Infinity : EVICT_CELLS_PER_PASS;
    const dropped = new Set<string>();
    const ids: string[] = [];
    const gone = new Set<string>();
    // The edges leaving with the dropped cells: the short ones paged by cell, and every link whose
    // out port is in a dropped window. Long edges go only when both ends do.
    const dead = new Map<number, Edge>();
    const goneEnd = (id: string) => gone.has(id) || !this.windows.has(id);
    let n = this.windows.size;
    let ne = this.edgeIds.size;
    for (const { k } of cands) {
      if ((n <= target && ne <= edgeTarget) || dropped.size >= maxCells) break;
      dropped.add(k);
      this.cellSeen.delete(k);
      const ws = this.cells.get(k);
      if (ws) {
        for (const w of ws) {
          gone.add(w.id);
          ids.push(w.id);
          n--;
        }
      }
      const es = this.edgeCells.get(k);
      if (es) for (const e of es) if (!dead.has(e.id)) (dead.set(e.id, e), ne--);
      if (ws) {
        for (const w of ws) {
          const own = this.edgesByWindow.get(w.id);
          if (!own) continue;
          for (const e of own) {
            if (dead.has(e.id) || e.from !== w.id) continue;
            const long = this.longSet.has(e.id);
            // Short intra-cluster edges are paged by the cell of their out port and leave only with it.
            if (!long && e.fromCluster === e.toCluster) continue;
            if (long && !goneEnd(e.to)) continue;
            dead.set(e.id, e);
            ne--;
          }
        }
      }
    }
    if (!dropped.size) return;
    this.evictSeq++;
    for (const k of dropped) this.evictedAt.set(k, this.evictSeq);
    // Paths of the dropped windows lose them (only those paths are touched).
    const paths = new Set<string>();
    for (const id of ids) {
      const w = this.windows.get(id);
      if (w) paths.add(w.path);
      this.windows.delete(id);
      this.outlines.delete(id);
      this.diffs.delete(id);
    }
    for (const p of paths) {
      const ws = this.windowsByPath.get(p);
      if (!ws) continue;
      const left = ws.filter((w) => !gone.has(w.id));
      if (left.length) this.windowsByPath.set(p, left);
      else this.windowsByPath.delete(p);
    }
    for (const k of dropped) {
      this.cells.delete(k);
      this.edgeCells.delete(k);
    }
    // Dead edges leave the per-window lists now (short; compacted in place, no garbage) and the
    // long cluster and pair lists lazily (marked stale).
    let longGone = false;
    const touched = new Set<string>();
    for (const e of dead.values()) {
      touched.add(e.from).add(e.to);
      this.staleLists.add("c" + e.fromCluster).add("c" + e.toCluster);
      if (e.fromCluster !== e.toCluster) this.staleLists.add("p" + e.fromCluster + ">" + e.toCluster);
      this.edgeIds.delete(e.id);
      if (this.longSet.has(e.id)) longGone = true;
    }
    for (const id of ids) this.edgesByWindow.delete(id);
    for (const k of touched) {
      const list = this.edgesByWindow.get(k);
      if (!list) continue;
      compact(list, this.live);
      if (!list.length) this.edgesByWindow.delete(k);
    }
    if (longGone) {
      this.longEdges = this.longEdges.filter((e) => !dead.has(e.id));
      this.longSet = new Set(this.longEdges.map((e) => e.id));
      this.listeners.longEdgesReset?.(this.longEdges);
    }
    // Links (labels, sites) whose windows are all gone.
    for (const l of this.links.values()) {
      const from = l.from.window;
      const to = "window" in l.to ? l.to.window : undefined;
      if (!(from && this.windows.has(from)) && !(to && this.windows.has(to))) this.links.delete(l.id);
    }
    this.loader.tell({ type: "forget", windows: ids, edges: [...dead.keys()] });
    for (const key of [...this.tilesDone]) {
      const [z, x, y] = key.split("/").map(Number);
      const size = TILE_BASE_WU * (1 << z);
      // Tiles overlapping a dropped cell (cells are CELL_WU squares keyed "cx,cy").
      for (let tx = Math.floor((x * size) / CELL_WU); tx <= Math.floor(((x + 1) * size - 1) / CELL_WU); tx++) {
        let hit = false;
        for (let ty = Math.floor((y * size) / CELL_WU); ty <= Math.floor(((y + 1) * size - 1) / CELL_WU); ty++) {
          if (dropped.has(cellKey(tx, ty))) {
            hit = true;
            break;
          }
        }
        if (hit) {
          this.tilesDone.delete(key);
          break;
        }
      }
    }
    this.lastEvictStats = { cells: dropped.size, windows: ids.length, edgeRefs: dead.size, ms: performance.now() - this.lastEvict, hard };
    this.listeners.evicted?.(dropped, ids);
  }
  /** Eviction passes so far, and the pass that last dropped each cell. */
  private evictSeq = 0;
  private evictedAt = new Map<string, number>();

  private evictedSince(r: Rect, seq: number) {
    if (this.evictSeq === seq) return false;
    for (const [x, y] of cellsOf(r)) if ((this.evictedAt.get(cellKey(x, y)) ?? -1) > seq) return true;
    return false;
  }

  /** What the last eviction dropped and how long it took (diagnostics). */
  lastEvictStats = { cells: 0, windows: 0, edgeRefs: 0, ms: 0, hard: false };

  /** Ids of long edges (never evicted). */
  private longSet = new Set<number>();

  /** The file-chip slot of a split file's windows (D14). */
  chipOf(w: WindowHeader): number {
    return w.fragment ? (this.chipSlots.get(w.path) ?? w.fragment.chip) : 0;
  }

  /**
   * Give a split file its chip slot once: the server's slot unless another split file nearby (within
   * about a screen at outline zoom) already wears it, then the next free one; when every slot is
   * taken nearby, the one least used in its own cell (D14 "the client shifts a slot when two split
   * files in view collide"). The header's slot is rewritten so every consumer sees the same colour.
   */
  private assignChip(w: WindowHeader) {
    if (!w.fragment) return;
    const cx = Math.floor(w.rect.x / CHIP_CELL_WU);
    const cy = Math.floor(w.rect.y / CHIP_CELL_WU);
    const k = cellKey(cx, cy);
    let mine = this.chipByCell.get(k);
    if (!mine) this.chipByCell.set(k, (mine = new Map()));
    const have = this.chipSlots.get(w.path);
    if (have !== undefined) {
      w.fragment.chip = have;
      mine.set(w.path, have);
      return;
    }
    this.serverChip.set(w.path, w.fragment.chip);
    const used = new Set<number>();
    for (let dx = -1; dx <= 1; dx++) {
      for (let dy = -1; dy <= 1; dy++) {
        for (const [p, s] of this.chipByCell.get(cellKey(cx + dx, cy + dy)) ?? []) if (p !== w.path) used.add(s);
      }
    }
    let slot = w.fragment.chip % FILE_CHIP_SLOTS;
    if (used.size >= FILE_CHIP_SLOTS) {
      const n = new Array<number>(FILE_CHIP_SLOTS).fill(0);
      for (const [p, s] of mine) if (p !== w.path) n[s]++;
      for (let i = 1; i < FILE_CHIP_SLOTS; i++) {
        const c = (w.fragment.chip + i) % FILE_CHIP_SLOTS;
        if (n[c] < n[slot]) slot = c;
      }
    } else for (let i = 0; i < FILE_CHIP_SLOTS && used.has(slot); i++) slot = (slot + 1) % FILE_CHIP_SLOTS;
    this.chipSlots.set(w.path, slot);
    mine.set(w.path, slot);
    w.fragment.chip = slot;
  }
  /** The server's chip slot per split file, before any shift (diagnostics). */
  readonly serverChip = new Map<string, number>();

  /** Stripe slot of a range commit (in-range blame), stable for the view. */
  ownerSlot(owner: string): number {
    let s = this.owners.get(owner);
    if (s === undefined) this.owners.set(owner, (s = this.owners.size));
    return s;
  }

  /** Owners seen so far in this range view, in slot order. */
  get ownerList(): string[] {
    return [...this.owners.keys()];
  }

  /** z so that one tile spans 512–1024 screen px (wire.ts TILE_BASE_WU). */
  tileLevel(zoom: number) {
    return Math.max(0, Math.ceil(Math.log2(512 / (TILE_BASE_WU * zoom))));
  }

  private tileCovered(z: number, x: number, y: number) {
    for (let zz = z; zz <= z + 8; zz++) {
      const s = 1 << (zz - z);
      if (this.tilesDone.has(`${zz}/${Math.floor(x / s)}/${Math.floor(y / s)}`)) return true;
    }
    return false;
  }

  private requestTiles(region: Rect, zoom: number) {
    const z = this.tileLevel(zoom);
    const size = TILE_BASE_WU * (1 << z);
    const w = this.boot.world;
    const x0 = Math.max(Math.floor(w.x / size), Math.floor(region.x / size));
    const x1 = Math.min(Math.floor((w.x + w.w) / size), Math.floor((region.x + region.w) / size));
    const y0 = Math.max(Math.floor(w.y / size), Math.floor(region.y / size));
    const y1 = Math.min(Math.floor((w.y + w.h) / size), Math.floor((region.y + region.h) / size));
    for (let x = x0; x <= x1; x++) {
      for (let y = y0; y <= y1; y++) {
        const key = `${z}/${x}/${y}`;
        if (this.loader.has("t" + key) || this.tileCovered(z, x, y)) continue;
        const gen = this.gen;
        const seq = this.evictSeq;
        const region = { x: x * size, y: y * size, w: size, h: size };
        this.loader
          .request({
            key: "t" + key,
            // The layout version busts cached tiles after a move or pin.
            url: `${this.base}/views/${this.view.id}/tiles/${key}?lv=${this.view.layoutVersion}`,
            kind: "tile",
            region,
            rank: 0,
            onPart: (ws) => gen === this.gen && this.addWindows(ws as WindowHeader[]),
          })
          .then((d) => {
            if (gen !== this.gen) return;
            // A cell under the tile was evicted while it streamed in: some of its windows are gone
            // again, so the tile is not done (it reloads when the camera comes back).
            if (!this.evictedSince(region, seq)) this.tilesDone.add(key);
            this.addEdges(d as PackedTile);
          })
          .catch((e) => void (gen === this.gen && this.noteFailure(e)));
      }
    }
  }

  private addWindows(ws: WindowHeader[]) {
    const changed = new Set<string>();
    const added: string[] = [];
    for (const w of ws) {
      if (this.windows.has(w.id)) continue;
      added.push(w.id);
      this.assignChip(w);
      this.windows.set(w.id, w);
      const byPath = this.windowsByPath.get(w.path);
      if (byPath) byPath.push(w);
      else this.windowsByPath.set(w.path, [w]);
      const k = cellKey(Math.floor(w.rect.x / CELL_WU), Math.floor(w.rect.y / CELL_WU));
      let list = this.cells.get(k);
      if (!list) this.cells.set(k, (list = []));
      list.push(w);
      changed.add(k);
    }
    if (added.length) this.loader.tell({ type: "have", windows: added, edges: [] });
    if (changed.size) this.listeners.windows?.(changed);
    // Arrivals between camera updates can cross the cap: enforce it right away.
    if (this.lastView && this.overCap()) this.evict(this.lastView, true);
    if (this.windows.size > this.peakWindows) this.peakWindows = this.windows.size;
  }

  private addEdges({ edges: packed, ends }: PackedTile) {
    const changed = new Set<string>();
    const long: Edge[] = [];
    const added: number[] = [];
    const push = (m: Map<string, Edge[]>, k: string, e: Edge) => {
      const l = m.get(k);
      if (l) l.push(e);
      else m.set(k, [e]);
    };
    for (let o = 0, i = 0; o < packed.length; o += EDGE_STRIDE, i += 5) {
      const id = packed[o];
      if (this.edgeIds.has(id)) continue;
      added.push(id);
      const e: Edge = {
        id,
        kind: LINK_KINDS[packed[o + 1]],
        from: ends[i],
        to: ends[i + 1],
        fromCluster: ends[i + 2],
        toCluster: ends[i + 3],
        linkId: ends[i + 4],
        ax: packed[o + 2],
        ay: packed[o + 3],
        bx: packed[o + 4],
        by: packed[o + 5],
        confidence: packed[o + 6],
      };
      this.edgeIds.set(id, e);
      push(this.edgesByWindow, e.from, e);
      // A link within one window (a local call) is listed under it once.
      if (e.to !== e.from) push(this.edgesByWindow, e.to, e);
      push(this.edgesByCluster, e.fromCluster, e);
      if (e.toCluster !== e.fromCluster) {
        push(this.edgesByCluster, e.toCluster, e);
        // Cross-cluster links draw as bundles (bootstrap) until something is selected.
        push(this.edgesByPair, e.fromCluster + ">" + e.toCluster, e);
        continue;
      }
      // Long low-confidence links (lexical candidates) are not paged: they show when their window or
      // cluster is selected (overlay), never as always-drawn hairlines across the map.
      if (Math.max(Math.abs(e.bx - e.ax), Math.abs(e.by - e.ay)) > LONG_EDGE_WU) {
        if (e.confidence < 0.6) continue;
        this.longSet.add(e.id);
        long.push(e);
        this.longEdges.push(e);
        continue;
      }
      // A short edge belongs to the cell of its out port.
      const k = cellKey(Math.floor(e.ax / CELL_WU), Math.floor(e.ay / CELL_WU));
      let list = this.edgeCells.get(k);
      if (!list) this.edgeCells.set(k, (list = []));
      list.push(e);
      changed.add(k);
    }
    if (added.length) this.loader.tell({ type: "have", windows: [], edges: added });
    if (changed.size) this.listeners.edges?.(changed);
    if (long.length) this.listeners.longEdges?.(long);
    this.listeners.edgesLoaded?.();
    if (this.lastView && this.overCap()) this.evict(this.lastView, true);
  }

  /** Whether outlines of some window in cell `k` are still being fetched. */
  outlinesPendingIn(k: string): boolean {
    if (!this.outlinePending.size) return false;
    for (const w of this.cells.get(k) ?? []) if (this.outlinePending.has(w.id)) return true;
    return false;
  }

  windowsIn(r: Rect): WindowHeader[] {
    const out: WindowHeader[] = [];
    // Windows are indexed by their top-left corner; look one cell up/left for tall or wide ones.
    for (const [cx, cy] of cellsOf(grow(r, CELL_WU, CELL_WU))) {
      for (const w of this.cells.get(cellKey(cx, cy)) ?? []) if (intersects(w.rect, r)) out.push(w);
    }
    return out;
  }

  private requestOutlines(r: Rect) {
    const need = this.windowsIn(r).filter((w) => !this.outlines.has(w.id) && !this.outlinePending.has(w.id));
    for (let i = 0; i < need.length; i += 200) {
      const chunk = need.slice(i, i + 200);
      const region = bounds(chunk.map((w) => w.rect));
      for (const w of chunk) this.outlinePending.add(w.id);
      const gen = this.gen;
      this.loader
        .request({
          key: "o" + chunk[0].id + ":" + chunk.length,
          url: `${this.base}/views/${this.view.id}/windows:batch`,
          method: "POST",
          body: { ids: chunk.map((w) => w.id), parts: this.isRange ? ["outline", "diff"] : ["outline"], layoutVersion: this.view.layoutVersion },
          kind: "batch",
          region,
          rank: 1,
        })
        .then((d) => {
          if (gen !== this.gen) return;
          const res = d as WindowBatchResponse;
          for (const df of res.diffs) {
            this.diffs.set(df.id, df);
            for (const l of df.lines) if (l.owner) this.ownerSlot(l.owner);
          }
          for (const l of res.links) this.links.set(l.id, l);
          const cells = new Set<string>();
          for (const o of res.outlines as unknown as DecodedOutline[]) {
            this.outlines.set(o.id, o);
            const w = this.windows.get(o.id);
            if (w) cells.add(cellKey(Math.floor(w.rect.x / CELL_WU), Math.floor(w.rect.y / CELL_WU)));
          }
          this.listeners.outlines?.(cells);
        })
        // Windows show no code while the server has lost the view: re-open it (viewLost).
        .catch((e) => void (gen === this.gen && this.noteFailure(e)))
        .finally(() => chunk.forEach((w) => this.outlinePending.delete(w.id)));
    }
  }

  /**
   * Re-fetch the headers of these windows (after an op: new blob ids, same places). Uses base-level
   * tiles around each window, bypassing the HTTP cache, and swaps in the changed headers.
   */
  async reloadHeaders(ids: string[]): Promise<WindowHeader[]> {
    const old = ids.map((id) => this.windows.get(id)).filter((w): w is WindowHeader => !!w);
    const keys = new Set<string>();
    for (const w of old) {
      for (let x = Math.floor(w.rect.x / TILE_BASE_WU); x <= Math.floor((w.rect.x + w.rect.w) / TILE_BASE_WU); x++) {
        keys.add(`0/${x}/${Math.floor(w.rect.y / TILE_BASE_WU)}`);
      }
    }
    const fresh = new Map<string, WindowHeader>();
    await Promise.all(
      [...keys].map(async (k) => {
        const r = await fetch(`${this.base}/views/${this.view.id}/tiles/${k}`, { cache: "no-store" });
        if (!r.ok) return;
        for (const w of ((await r.json()) as { windows: WindowHeader[] }).windows) fresh.set(w.id, w);
      }),
    );
    const changed: WindowHeader[] = [];
    for (const w of old) {
      const n = fresh.get(w.id);
      if (n && (n.blob !== w.blob || n.lines.start !== w.lines.start || n.lines.end !== w.lines.end)) {
        this.replaceWindow(w, n);
        changed.push(n);
      }
    }
    return changed;
  }

  /** Headers in the base tiles under `rects`, without adding them to the loaded set. */
  async peekHeaders(rects: Rect[]): Promise<WindowHeader[]> {
    const keys = new Set<string>();
    for (const r of rects) {
      for (let x = Math.floor(r.x / TILE_BASE_WU); x <= Math.floor((r.x + r.w) / TILE_BASE_WU); x++) {
        for (let y = Math.floor(r.y / TILE_BASE_WU); y <= Math.floor((r.y + r.h) / TILE_BASE_WU); y++) keys.add(`0/${x}/${y}`);
      }
    }
    const out = new Map<string, WindowHeader>();
    await Promise.all(
      [...keys].map(async (k) => {
        const r = await fetch(`${this.base}/views/${this.view.id}/tiles/${k}`);
        if (!r.ok) return;
        for (const w of ((await r.json()) as { windows: WindowHeader[] }).windows) out.set(w.id, w);
      }),
    );
    return [...out.values()];
  }

  /** Swap in a widened header (expand). It moves to the end of its cell so it draws on top. */
  replaceWindow(old: WindowHeader, nw: WindowHeader) {
    this.assignChip(nw);
    this.windows.set(nw.id, nw);
    const byPath = this.windowsByPath.get(nw.path);
    if (byPath) byPath[byPath.indexOf(old)] = nw;
    const k = cellKey(Math.floor(old.rect.x / CELL_WU), Math.floor(old.rect.y / CELL_WU));
    const list = this.cells.get(k);
    if (list) {
      list.splice(list.indexOf(old), 1);
      list.push(nw);
    }
    this.outlines.delete(nw.id);
  }

  /** Drop cached text and outlines of a file's windows (after an edit); they reload on demand. */
  forget(path: string): WindowHeader[] {
    const ws = this.windowsByPath.get(path) ?? [];
    for (const w of ws) {
      this.texts.delete(this.textKey(w));
      this.outlines.delete(w.id);
    }
    return ws;
  }

  textKey(w: WindowHeader) {
    return `${w.blob}:${w.lines.start}:${w.lines.end}`;
  }

  private requestTexts(r: Rect) {
    for (const w of this.windowsIn(r)) {
      const key = this.textKey(w);
      if (this.texts.has(key) || this.textPending.has(key)) continue;
      this.textPending.add(key);
      this.loader
        .request({
          key: "x" + key,
          url: `${this.base}/blobs/${w.blob}/text?from=${w.lines.start}&to=${w.lines.end}`,
          kind: "json",
          region: w.rect,
          rank: 2,
        })
        .then((d) => {
          this.texts.set(key, d as BlobText);
          this.listeners.text?.([w.id]);
        })
        .catch(() => {})
        .finally(() => this.textPending.delete(key));
    }
  }
}

/** Keep the entries of `list` that pass `keep`, in place (order kept, nothing allocated). */
function compact<T>(list: T[], keep: (x: T) => boolean) {
  let j = 0;
  for (let i = 0; i < list.length; i++) {
    const x = list[i];
    if (keep(x)) list[j++] = x;
  }
  list.length = j;
}

export function bounds(rs: Rect[]): Rect {
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (const r of rs) {
    x0 = Math.min(x0, r.x);
    y0 = Math.min(y0, r.y);
    x1 = Math.max(x1, r.x + r.w);
    y1 = Math.max(y1, r.y + r.h);
  }
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}
