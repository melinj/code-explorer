import { Application, Container, Graphics } from "pixi.js";
import type { CoChangeClusterPair, CoChangePair, Cluster, Lens, Rect, SnapshotOptions, StructureResponse, ViewLayout, WindowHeader, ZoomTier } from "../contracts/model";
import { CELL_WU, DataClient, bounds, cellKey, type Edge } from "../data/client";
import { grow, intersects } from "../data/loader";
import { buildCodeText, type ExpandHit } from "../windows/codeText";
import { Backdrop } from "./backdrop";
import { CoChange, MAX_ARCS, clusterPartners, coStrength, drawPartners, labelPartners, pct } from "./cochange";
import { GATHER, Gather, type GatherItem } from "./gather";
import { HEADER_WU, LINE_WU, TILE_BASE_WU } from "../contracts/wire";
import { Camera, MAX_ZOOM, clamp, tierIndex } from "./camera";
import { Chunk, cochangePaths, cochangeStrength, isHiddenContext, isMinimized, type Look, type Part } from "./chunks";
import { Structure, isTreeLens, layoutFor, setStructure, structure } from "./structure";
import { LabelBatch, fit, monoFont, sansFont, textWidth } from "./labels";
import { CLUSTER_LABEL, GLYPH_BORDER_PX, GLYPH_PX, clusterLabelRect, glyphSpot } from "./glyphSpot";
import { RectBatch, flushPalette, dispose, paletteUse } from "./rects";
import { COLHEAD_PX, Hud, RAIL_PX } from "./hud";
import { Bundles } from "./bundles";
import { EdgePages } from "./edgePages";
import { GROUP_OF, LOW_CONFIDENCE, curve, dashedPolyline, distSq, edgePoints, type LinkGroup } from "./edges";
import { Overlay, targetKey, type Target } from "./overlay";
import { describe } from "./describe";
import { Tooltip } from "./tooltip";
import { ThreadMarks } from "./threadMarks";
import { CommentRange, type LineRange } from "./commentRange";
import { Spokes } from "./spokes";

/** Most highways drawn per tier (L0, L1): the strongest by link count; L2+ draws them all. */
/** Highways drawn per tier, the busiest: zoomed in, individual links take over and highways are context. */
const HIGHWAY_CAP = [150, 500, 1500, 1500];
import { Presence, type Attention } from "./presence";
import type { ReviewClient } from "../data/review";
import type { ConflictDetail, EditResult, RangeOptions, Thread, ViewSpec } from "../contracts/model";
import { GUTTER_WU } from "./chunks";
import { CHIP_HI, CHIP_REST, DARK, clusterHeatLevel, heatColor, heatLevel, mix, step, type Theme } from "./theme";
import type { FragmentsResponse } from "../contracts/http";

export type CanvasAction =
  | { type: "thread"; thread: Thread; window: WindowHeader }
  | { type: "compose"; window: WindowHeader; line: number; end?: number }
  | { type: "edit"; window: WindowHeader }
  | { type: "conflict"; conflict: ConflictDetail; window: WindowHeader };

export interface EngineStats {
  fps: number;
  frameP50: number;
  frameP95: number;
  frameMax: number;
  tier: ZoomTier;
  zoom: number;
  windowsLoaded: number;
  windowsTotal: number;
  windowsVisible: number;
  chunksVisible: number;
  pendingRequests: number;
  pendingBuilds: number;
  /** A view switch (range, snapshot, a reload in place) is between its request and its rebuild. */
  pendingSwitch: boolean;
  buildMsLastSecond: number;
  /** Rect palette cells in use, texture uploads so far, and the atlas capacity. */
  palette: { cells: number; uploads: number; capacity: number };
}

/** Zoom each tier's strokes are sized for (strokes are world-space and rebuilt on tier change). */
const STROKE_ZOOM: Record<ZoomTier, number> = { L0: 0.01, L1: 0.05, L2: 0.2, L3: 1 };
const BUDGET_IDLE_MS = 10;
const BUDGET_MOVING_MS = 4;
/** Part builds are not started or resumed with less than this left of the frame's build budget. */
const BUILD_MIN_SLICE_MS = 0.5;
const SETTLE_MS = 160;
const DATA_THROTTLE_MS = 100;
/**
 * Outline batches for a cell mark its silhouettes and pills dirty once the cell has none in flight,
 * or this long after the first unapplied batch: not once per batch (a dense chunk got 4–6 rebuilds).
 */
const OUTLINE_COALESCE_MS = 120;

type LabelMode = "none" | "titles" | "heads";

/**
 * F1 canvas engine. One render-on-demand loop: camera motion moves two world transforms and the
 * HUD; chunk geometry is rebuilt only when its data or look changes, in time-boxed slices that
 * shrink while the camera moves. Rendering never waits on the network.
 */
export class Engine {
  readonly cam = new Camera();
  readonly app = new Application();
  private worldBack = new Container({ isRenderGroup: true });
  private worldFront = new Container({ isRenderGroup: true });
  private chunkLayer = new Container();
  private edgeLayer = new Container();
  private backdrop = new Backdrop();
  private hud = new Hud();
  /** L3 code text per window, parented to the window's chunk. */
  private code = new Map<string, { c: Container; chunk: Chunk; born: number; hits: ExpandHit[] }>();
  /** L3 code text meshes kept at most; past it the oldest off-screen ones go (they rebuild from cached text). */
  codeCap = 160;
  private codeQueue = new Set<string>();
  /** Cells with outline batches not yet applied, and when the first of them arrived. */
  private outlineCells = new Map<string, number>();
  private fading: Array<{ c: Container; born: number }> = [];
  private chunks = new Map<string, Chunk>();
  private visible = new Set<Chunk>();
  private visibleEdges = new Set<Chunk>();
  private longEdges = new EdgePages();
  private bundles = new Bundles();
  private overlay = new Overlay();
  private tooltip = new Tooltip();
  private hover?: Target;
  selected?: Target;
  private overlayDirty = false;
  private focusActive = false;
  private overlayRegion?: Rect;
  private pointer?: { x: number; y: number; moved: boolean };
  private onSelectCb?: (t: Target | undefined) => void;
  private threadMarks = new ThreadMarks();
  private rangeMark = new CommentRange();
  /** A line the side panel points at (hovering a link row): its band, and its link highlighted. */
  private peekMark = new CommentRange();
  private peekDirty = false;
  private spokes = new Spokes();
  /** Outline of a window or cluster being dragged to a new (pinned) place. */
  private moveGhost = new Graphics();
  private spokesDirty = true;
  private rangeDirty = false;
  /** The commented range changed by dragging one of its handles. */
  onCommentRange?: (r: LineRange) => void;
  /** Co-change pairs of the view on screen: fetched when the lens first turns on, dropped on a view switch. */
  private cochange?: CoChange;
  private cochangeView?: string;
  private cochangeDirty = false;
  private cochangeLayer = new Container();
  /** Co-change labels, above the windows (the glows, rings and arcs stay beneath them). */
  private cochangeTop = new Container();
  /** Co-change gather (gather.ts): the focus's partners pulled into a tray beside it. */
  private gather = new Gather();
  /** Where the pointer came to rest and when (gather dwell), and the focus Esc dismissed. */
  private still?: { x: number; y: number; at: number };
  private gatherDismissed?: string;
  private dwellFocus?: string;
  private gatherLeft = 0;
  private dwellCue = document.createElement("div");
  /** Range views: context windows expanded in place. */
  private expanded = new Set<string>();
  /** The selected file's fragments (fragments route), for dotted fragment links and ]f / [f. */
  private fragments?: FragmentsResponse;
  private fragLayer = new Container();
  private fragDirty = false;
  /** Dotted fragment links drawn for the selected file (e2e checks). */
  fragLinks = 0;
  /**
   * L2 district glyphs (§4.9), sized in screen pixels: a window header is a few pixels tall at
   * outline zoom, so the glyph sits beside the window's top-left corner instead, clear of other
   * windows and cluster labels (glyphSpot.ts). Rebuilt for
   * the windows around the view when the camera settles at another zoom or place.
   */
  private glyphLayer = new Container();
  private glyphDirty = true;
  private glyphBuilt?: { zoom: number; region: Rect };
  /** Glyphs drawn, their size on screen, and how many could not take the corner spot (e2e). */
  glyphStats = { count: 0, px: 0, moved: 0 };
  /** Where the last build put each glyph (world units; e2e checks they cover no label or window). */
  glyphRects: Array<Rect & { id: string }> = [];
  readonly presence = new Presence();
  private presenceDirty = false;
  private presenceLast = 0;
  private threadsDirty = true;
  private viewListeners = new Set<() => void>();
  /** Canvas actions the panels answer: open a thread, start one at a line, edit a window. */
  onAction?: (a: CanvasAction) => void;
  private look: Look;
  private tier: ZoomTier = "L0";
  private labelMode: LabelMode = "none";
  private camVersion = -1;
  private partsTier = -1;
  private dirty = true;
  private lastData = 0;
  private lastCodeScan = 0;
  private frameTimes: number[] = [];
  private lastFrame = performance.now();
  private buildLog: Array<[number, number]> = [];
  private raf = 0;
  private wheel?: { log: number; sx: number; sy: number };
  private onStats?: (s: EngineStats) => void;
  private lastStats = 0;
  /** Extra per-frame hook (benchmarks drive the camera through it). */
  beforeFrame?: (now: number, dt: number) => void;

  constructor(
    private el: HTMLElement,
    readonly client: DataClient,
    readonly review: ReviewClient,
  ) {
    review.onChange(() => {
      this.threadsDirty = true;
    });
    this.look = {
      theme: DARK,
      strokeZoom: STROKE_ZOOM.L0,
      tierIndex: 0,
      hidden: new Set(),
      lens: "none",
      windowOf: (id) => this.client.windows.get(id),
      diffOf: (id) => this.client.diffs.get(id),
      linkOf: (id) => this.client.links.get(id),
      expanded: this.expanded,
    };
  }

  async start(onStats?: (s: EngineStats) => void, onSelect?: (t: Target | undefined) => void) {
    this.onStats = onStats;
    this.onSelectCb = onSelect;
    // The first view's structure (later views load theirs on the switch).
    if (isTreeLens(this.look.lens)) void this.loadStructure();
    await this.app.init({
      resizeTo: this.el,
      autoStart: false,
      sharedTicker: false,
      antialias: true,
      autoDensity: true,
      resolution: devicePixelRatio,
      background: this.look.theme.bg,
      preference: "webgl",
      powerPreference: "high-performance",
    });
    this.app.ticker.stop();
    this.dwellCue.className = "cv-dwell";
    this.dwellCue.hidden = true;
    this.el.append(this.app.canvas, this.tooltip.el, this.dwellCue);
    // Links run beneath windows, so they never cross code text; ports sit on the window borders.
    // Highways run beneath the clusters too (L0 fills, L1+ cards), so they show only in the gutters.
    // Above the windows are only outlines and marks, never a link.
    this.worldFront.addChild(this.bundles.root, this.backdrop.cards, this.backdrop.front, this.longEdges.root, this.spokes.root, this.edgeLayer, this.overlay.links, this.fragLayer, this.cochangeLayer, this.chunkLayer, this.glyphLayer, this.cochangeTop, this.peekMark.root, this.rangeMark.root, this.threadMarks.root, this.overlay.root, this.presence.root, this.moveGhost, this.gather.root);
    this.bundles.setData(this.client.boot);
    this.app.stage.addChild(this.worldBack, this.hud.under, this.worldFront, this.hud.over);
    this.worldBack.addChild(this.backdrop.back);

    this.client.on({
      windows: (cells) => {
        const t0 = performance.now();
        for (const k of cells) this.chunk(k).setWindows(this.client.cells.get(k) ?? []);
        this.threadsDirty = true;
        this.glyphDirty = true;
        // Windows that arrive around a selection are dimmed too (D14): redraw the focus.
        if (this.selected) this.overlayDirty = true;
        if (this.look.lens === "cochange") this.cochangeDirty = true;
        this.mark("tile-in", t0);
        this.dirty = true;
      },
      edges: (cells) => {
        for (const k of cells) this.chunk(k).setEdges(this.client.edgeCells.get(k) ?? []);
        this.dirty = true;
      },
      edgesLoaded: () => {
        // Newly loaded links may belong to what is selected or hovered.
        if (this.selected || this.hover) {
          this.overlayDirty = true;
          if (this.selected) this.onSelectCb?.(this.selected);
        }
      },
      longEdges: (edges) => {
        this.longEdges.add(edges);
      },
      outlines: (cells) => {
        const now = performance.now();
        for (const k of cells) if (!this.outlineCells.has(k)) this.outlineCells.set(k, now);
      },
      text: (ids) => {
        for (const id of ids) this.codeQueue.add(id);
      },
      longEdgesReset: (edges) => {
        this.longEdges.reset();
        this.longEdges.add(edges);
      },
      evicted: (cells, ids) => {
        for (const k of cells) {
          const c = this.chunks.get(k);
          if (!c) continue;
          this.visible.delete(c);
          this.visibleEdges.delete(c);
          c.destroy();
          this.chunks.delete(k);
        }
        for (const id of ids) {
          const x = this.code.get(id);
          if (x) dispose(x.c);
          this.code.delete(id);
          this.codeQueue.delete(id);
        }
        this.camVersion = -1;
        this.glyphDirty = true;
        this.dirty = true;
      },
      viewLost: () => void this.recoverView(),
    });

    const boot = this.client.boot;
    this.setTheme(this.look.theme);
    this.cam.resize(this.el.clientWidth, this.el.clientHeight);
    this.home(0);
    new ResizeObserver(() => {
      this.app.resize();
      this.cam.resize(this.el.clientWidth, this.el.clientHeight);
    }).observe(this.el);
    this.bindInput();
    this.raf = requestAnimationFrame(this.frame);
    return boot;
  }

  /**
   * Render every backdrop layer once before the first frame, so shader compilation and the first
   * buffer uploads happen during startup instead of the first time a tier is reached.
   */
  private warmUp() {
    const t0 = performance.now();
    this.backdrop.showAll();
    // Both glyph atlases: their textures upload (with mipmaps) on first use.
    const probe = new Container();
    for (const f of [sansFont(), monoFont()]) {
      const b = new LabelBatch(f);
      b.add("warm", this.cam.x, this.cam.y, 20, 0xffffff);
      const c = new Container();
      b.build(c);
      probe.addChild(c);
    }
    const rb = new RectBatch();
    rb.add(this.cam.x, this.cam.y, 10, 10, 0xffffff);
    const rc = new Container();
    rb.build(rc);
    probe.addChild(rc);
    this.worldFront.addChild(probe);
    flushPalette();
    this.app.render();
    dispose(probe);
    this.backdrop.setTier(tierIndex(this.tier));
    this.warmUpMs = performance.now() - t0;
  }
  warmUpMs = 0;

  destroy() {
    cancelAnimationFrame(this.raf);
    this.app.destroy(true, { children: true });
  }

  setTheme(t: Theme) {
    this.releaseGather(true);
    this.look = { ...this.look, theme: t, churn: this.client.boot.churn };
    this.app.renderer.background.color = t.bg;
    this.backdrop.build(this.client.boot, t);
    this.spokes.setBoot(this.client.boot);
    this.spokesDirty = true;
    this.tessellate(this.backdrop.back);
    this.tessellate(this.backdrop.front);
    this.backdrop.setTier(tierIndex(this.tier));
    this.hud.build(this.client.boot, t);
    for (const { c } of this.code.values()) dispose(c);
    this.code.clear();
    this.lastCodeScan = 0;
    for (const c of this.chunks.values()) for (const p of ["frames", "edges", "titles", "heads", "sil", "ports"] as Part[]) c.dirty.add(p);
    this.longEdges.invalidate();
    this.restyleBundles();
    this.overlayDirty = true;
    this.glyphDirty = true;
    this.camVersion = -1;
    this.dirty = true;
    this.warmUp();
  }

  /** Rebuild the highways for the current tier, filters and lens (budgeted pages). */
  private restyleBundles() {
    const l = this.look;
    const fades = l.lens !== "none" || l.brush ? (c: Cluster) => this.backdrop.fadesCluster(c) : undefined;
    this.bundles.restyle(l.theme, 1 / l.strokeZoom, l.hidden, l.tierIndex <= 1 ? 2 : 1, HIGHWAY_CAP[l.tierIndex] ?? Infinity, this.focusActive, fades, l.tierIndex <= 1);
  }

  /** Hide or show link groups (legend filters). Rebuilds links in budgeted slices. */
  setHiddenGroups(hidden: ReadonlySet<LinkGroup>) {
    this.look = { ...this.look, hidden };
    this.longEdges.hidden = hidden;
    this.longEdges.invalidate();
    this.spokesDirty = true;
    this.restyleBundles();
    for (const c of this.chunks.values()) {
      c.dirty.add("edges");
      c.dirty.add("ports");
    }
    this.overlayDirty = true;
    this.dirty = true;
  }

  /** Told when the engine opens a lane itself ([goTo] into a closed lane), so the lane pills follow. */
  onHiddenLanes?: (lanes: ReadonlySet<string>) => void;

  /** Lane filters: hide a lane's windows and code (layout unchanged). */
  setHiddenLanes(lanes: ReadonlySet<string>) {
    this.look = { ...this.look, hiddenLanes: lanes };
    this.backdrop.setHiddenLanes(lanes);
    this.tessellate(this.backdrop.back);
    this.tessellate(this.backdrop.front);
    for (const c of this.chunks.values()) for (const p of ["frames", "sil", "ports", "titles", "heads"] as Part[]) c.dirty.add(p);
    for (const [id, e] of this.code) {
      const w = this.client.windows.get(id);
      e.c.visible = !w || !lanes.has(w.lane);
    }
    this.glyphDirty = true;
    this.dirty = true;
  }

  /**
   * Emphasis lens (§4.9): heat fades cold windows and glows under hot ones; connectedness fades
   * isolated windows, weighs links by coupling and raises central windows; hotspots keeps only
   * review-first windows at full strength; co-change draws "changes together" links; flow keeps
   * code linked across its directory's edge; tension, busy files among quiet ones. The L0 cluster
   * map follows the same lens. A snapshot also takes the lens's layout (structure.ts layoutFor):
   * none, flow and tension show the folder tree, the others the district grid.
   */
  setLens(lens: Lens) {
    if (this.look.lens === lens) return;
    this.releaseGather(true);
    this.look = { ...this.look, lens };
    // Cards take the lens's colours; links and highways whose ends it fades step back with them;
    // the structure lenses badge the titles and headers.
    for (const c of this.chunks.values()) for (const p of ["frames", "edges", "titles", "heads"] as Part[]) c.dirty.add(p);
    this.restyleBundles();
    this.backdrop.setLens(lens, this.look.brush);
    this.glyphDirty = true;
    this.dirty = true;
    if (lens === "cochange") void this.loadCochange();
    this.cochangeFor = undefined;
    this.backdrop.setCochangeFocus(undefined);
    this.noteCochangeFocus();
    this.cochangeDirty = true;
    // Under the co-change lens, hovering shows co-change rather than static links (overlay).
    if (this.hover) this.overlayDirty = true;
    if (isTreeLens(lens)) void this.loadStructure();
    void this.applyLayout();
  }

  /** The layout snapshots are opened in: the lens's (see setLens). */
  private layout: ViewLayout = "folders";

  /** A snapshot spec with the current layout; other specs as they are. */
  private withLayout(spec: unknown): unknown {
    const s = spec as ViewSpec;
    return s.mode === "snapshot" ? { ...s, options: { ...s.options, layout: this.layout } } : spec;
  }

  /** The layout the lens shows, for the snapshot on screen: switch views, keeping the selected window in sight. */
  private async applyLayout() {
    const layout = layoutFor(this.look.lens);
    if (layout === this.layout) return;
    this.layout = layout;
    const vs = this.client.view.spec as ViewSpec;
    // Range views keep the map of their tip (D16); the next snapshot opens in this layout.
    if (vs.mode !== "snapshot") return;
    const sel = this.selected?.type === "window" ? this.selected.w : undefined;
    const moved = ((v) => () => this.cam.version !== v)(this.cam.version);
    if (!(await this.switchView(vs))) return;
    // Back to the selection (or home), unless the camera moved meanwhile: a pan or a fly started
    // during the switch wins over this one.
    if (moved()) return;
    if (!sel) return this.home(600);
    const at = (await this.locate(sel.path))[0];
    if (at && !moved()) await this.goTo({ path: sel.path, line: sel.lines.start });
  }

  /** The folder-tree lenses: the view's structure (GET /views/:id/structure), once per view. */
  private structureView?: string;

  /** Called with the structure of the view on screen when it arrives (undefined on a view switch). */
  onStructure?: (s?: StructureResponse) => void;

  private async loadStructure() {
    // Snapshots only: a range view keeps its tip's map, and /structure does not know its id (a 404
    // there would read as a lost view and re-open it).
    if (this.isRange) return;
    const view = this.client.view.id;
    if (structure && this.structureView === view) return;
    const r = await fetch(`/api/v1/views/${view}/structure`);
    if (!r.ok) return void this.client.noteFailure(r);
    const s = (await r.json()) as StructureResponse;
    if (this.client.view.id !== view) return;
    setStructure(new Structure(s));
    this.structureView = view;
    this.onStructure?.(s);
    for (const c of this.chunks.values()) for (const p of ["frames", "edges", "titles", "heads"] as Part[]) c.dirty.add(p);
    this.backdrop.structureChanged();
    this.restyleBundles();
    this.glyphDirty = true;
    this.dirty = true;
  }

  private async loadCochange() {
    const view = this.client.view.id;
    if (this.cochange && this.cochangeView === view) return;
    const r = await fetch(`/api/v1/views/${view}/metrics?level=file&limit=1`);
    if (!r.ok) return void this.client.noteFailure(r);
    const m = (await r.json()) as { cochange: CoChangePair[]; cochangeClusters?: CoChangeClusterPair[] };
    if (this.client.view.id === view) this.setCochange(new CoChange(m.cochange, m.cochangeClusters));
  }

  /** Install (or drop) the co-change pairs: the file set chunks fade by, the L0 map, the focus layer. */
  private setCochange(co?: CoChange) {
    this.cochange = co;
    this.cochangeView = co ? this.client.view.id : undefined;
    cochangePaths.clear();
    cochangeStrength.clear();
    for (const p of co?.paths ?? []) cochangePaths.add(p);
    for (const [path, row] of co?.files ?? []) {
      let k = 0;
      for (const p of row.values()) k = Math.max(k, coStrength(p, "file"));
      cochangeStrength.set(path, k);
    }
    this.backdrop.setCochange(co);
    if (this.look.lens === "cochange") this.restyleBundles();
    for (const c of this.chunks.values()) {
      c.dirty.add("frames");
      c.dirty.add("edges");
    }
    this.cochangeDirty = true;
    this.dirty = true;
  }

  /**
   * What the co-change lens is about: the file under the pointer (else the selected one), or a
   * cluster when the pointer is on a cluster rather than a window ("c:<id>" / "f:<path>").
   */
  private cochangeFocus(): string | undefined {
    const of = (t?: Target) =>
      t?.type === "window"
        ? "f:" + t.w.path
        : t?.type === "port"
          ? "f:" + t.p.window.path
          : t?.type === "thread" || t?.type === "conflict" || t?.type === "expand"
            ? "f:" + t.h.window.path
            : t?.type === "cluster"
              ? "c:" + t.c.id
              : undefined;
    return of(this.hover) ?? of(this.selected);
  }

  /** Co-change lens: whose set is drawn now, so a pointer move inside it rebuilds nothing. */
  private cochangeFor?: string;

  private noteCochangeFocus() {
    if (this.look.lens !== "cochange") return;
    // L0 (the backdrop): the cluster pointed at, or the one the pointed-at window sits in.
    const cl = (t?: Target) =>
      t?.type === "cluster" ? t.c.id : t?.type === "window" ? t.w.cluster : t?.type === "port" ? t.p.window.cluster : t?.type === "thread" || t?.type === "conflict" || t?.type === "expand" ? t.h.window.cluster : undefined;
    this.backdrop.setCochangeFocus(cl(this.hover) ?? cl(this.selected));
    const f = this.cochangeFocus();
    if (f === this.cochangeFor) return;
    this.cochangeFor = f;
    this.cochangeDirty = true;
    this.dirty = true;
  }

  /** What the last co-change focus drew (e2e checks). */
  cochangeStats = { partners: 0, arcs: 0, offscreen: 0 };

  /**
   * Co-change lens from L1 on. Pointing at a file (or selecting it) lights up the files that change
   * with it; pointing at a cluster box, the clusters. Partners glow and get a ring by how surely
   * they change together (coStrength: jaccard with its support behind it), an arc runs from the
   * focus to each (dashed where no static link explains the coupling), and from L2 each is labelled
   * with its jaccard. A partner whose windows are not loaded is shown by its cluster, so the arc
   * still points where it lives. At L0 the backdrop draws the cluster level.
   */
  private buildCochange() {
    const g = this.cochangeLayer;
    for (const ch of g.removeChildren()) dispose(ch);
    for (const ch of this.cochangeTop.removeChildren()) dispose(ch);
    this.cochangeStats = { partners: 0, arcs: 0, offscreen: 0 };
    const focus = this.cochangeFor;
    const co = this.cochange;
    if (this.look.lens !== "cochange" || !co || !focus || this.tier === "L0") return;
    const t = this.look.theme;
    const px = 1 / this.look.strokeZoom;
    const dark = t.name === "dark";
    const boot = this.client.boot;
    const byId = new Map<string, (typeof boot.clusters)[number]>(boot.clusters.map((c) => [c.id, c]));
    const card = (r: Rect) => grow(r, 30);
    const rb = new RectBatch(256);
    const labels = new LabelBatch(sansFont());
    const items: Array<{ rect: Rect; text: string; strong: boolean }> = [];
    let focusRect: Rect | undefined;
    const partners: Array<{ rect: Rect; p: { jaccard: number; support: number; linked: boolean }; k: number }> = [];
    if (focus.startsWith("c:")) {
      const f = byId.get(focus.slice(2));
      if (!f) return;
      focusRect = card(f.rect);
      items.push({ rect: focusRect, text: f.label, strong: true });
      for (const p of clusterPartners(co, f.id, byId)) {
        const r = card(p.rect);
        // A faint wash over the partner's card, beneath its windows.
        rb.round(r.x, r.y, r.w, r.h, 60, t.couple, 0.06 + 0.14 * step(p.k));
        partners.push({ rect: r, p: p.p, k: p.k });
        items.push({ rect: r, text: `${p.c.label}  ${pct(p.p.jaccard)}`, strong: false });
      }
    } else {
      const path = focus.slice(2);
      const own = this.client.windowsByPath.get(path) ?? [];
      if (!own.length) return;
      focusRect = own[0].rect;
      for (const w of own.slice(1)) rb.ring(w.rect.x - 4 * px, w.rect.y - 4 * px, w.rect.w + 8 * px, w.rect.h + 8 * px, 2 * px, dark ? 0xffffff : t.ink, 0.7);
      items.push({ rect: focusRect, text: basename(path), strong: true });
      const list = [...(co.files.get(path) ?? [])].map(([q, p]) => ({ q, p, k: coStrength(p, "file") })).sort((a, b) => b.k - a.k);
      for (const { q, p, k } of list) {
        const ws = this.client.windowsByPath.get(q) ?? [];
        if (ws.length) {
          for (const w of ws) {
            const r = w.rect;
            rb.soft(r.x, r.y, r.w, r.h, (8 + 34 * k) * px + Math.min(r.w, r.h) * 0.04 * k, t.couple, (dark ? 0.28 : 0.2) + (dark ? 0.5 : 0.4) * step(k));
          }
          partners.push({ rect: ws[0].rect, p, k });
          items.push({ rect: ws[0].rect, text: `${basename(q)}  ${pct(p.jaccard)}`, strong: false });
        } else {
          // Not loaded (off-screen): point at the cluster it lives in.
          const c = byId.get(co.fileCluster.get(q) ?? "");
          if (!c) continue;
          partners.push({ rect: card(c.rect), p, k });
          items.push({ rect: card(c.rect), text: `${basename(q)}  ${pct(p.jaccard)}`, strong: false });
          this.cochangeStats.offscreen++;
        }
      }
    }
    drawPartners(rb, t, px, focusRect, partners, 2 * px);
    this.cochangeStats.partners = partners.length;
    this.cochangeStats.arcs = Math.min(partners.length, MAX_ARCS);
    // Labels where they can be read: from L2 for files, from L1 for clusters.
    const pills = new RectBatch(64);
    if (focus.startsWith("c:") || tierIndex(this.tier) >= 2) labelPartners(pills, labels, t, 1 / this.cam.zoom, items, "above");
    const c = new Container();
    rb.build(c);
    g.addChild(c);
    const p = new Container();
    pills.build(p);
    const l = new Container();
    labels.build(l);
    this.cochangeTop.addChild(p, l);
    this.builtCochangeZoom = this.cam.zoom;
  }
  /** Zoom the co-change labels were sized for (they are rebuilt when the camera settles far from it). */
  private builtCochangeZoom = 0;

  /**
   * Open a range view on the same map (D16): only what the range changes, plus context windows shown
   * as the options say (minimized pills by default). The camera frames what changed.
   */
  async openRange(revset: string, options: Partial<RangeOptions> & { allowNonChain?: boolean }, fileset?: string): Promise<boolean> {
    const opts = { diff: "cumulative", contextHops: 1, contextDisplay: "minimized", contextLines: 3, ignoreWhitespace: false, ...options };
    if (!(await this.switchView({ mode: "range", revset, ...(fileset ? { fileset } : {}), options: opts }))) return false;
    this.fit(600);
    return true;
  }

  /** Clusters a range view changes (bootstrap clusters with windows); empty outside range views. */
  changedClusters() {
    return this.isRange ? this.client.boot.clusters.filter((c) => c.windows > 0) : [];
  }

  /**
   * Open a snapshot (DESIGN §7): `@` by default, any revision that resolves to one commit, or a
   * workspace's `@` with its live files (`options.workspace`). Edits there go where §6 says for that
   * commit (amend it, insert after, branch; immutable commits only branch). Throws the server's
   * message when it refuses the spec (bad revset, unknown workspace); the current view stays.
   */
  async openSnapshot(rev = "@", options?: SnapshotOptions) {
    const spec = { mode: "snapshot", rev, ...(options && Object.keys(options).length ? { options } : {}) };
    if (await this.switchView(spec)) this.home(600);
  }

  /** The revision the snapshot on screen was opened at (`@`, `gym@`, a change id…); undefined in a range view. */
  get snapshotRev(): string | undefined {
    const spec = this.client.view?.spec as { mode: string; rev?: string } | undefined;
    return spec?.mode === "snapshot" ? (spec.rev ?? "@") : undefined;
  }

  /** The commit the view shows (snapshot: its head). */
  get viewHead(): string | undefined {
    return this.client.view?.resolved.head;
  }

  /** Called with a short notice when the view was re-opened (the server lost it, e.g. a restart). */
  onViewRecovered?: (msg: string) => void;
  private recovering = false;

  /**
   * The server answered 404 "view-unknown" for the view on screen (http.ts "Views after a restart"):
   * re-POST its spec and reload in place, keeping the camera. Windows would otherwise show no code.
   */
  async recoverView() {
    if (this.recovering) return;
    this.recovering = true;
    try {
      const old = this.client.view.id;
      await this.reloadLayout();
      if (this.client.view.id !== old) this.onViewRecovered?.("The server no longer had this view (it restarted?): reopened it.");
    } catch (e) {
      this.onViewRecovered?.(`The server lost this view and reopening it failed: ${(e as Error).message}`);
    } finally {
      this.recovering = false;
    }
  }

  /**
   * After the event socket reconnects (the server may have restarted): if the view is gone
   * (404 view-unknown) or no longer what its spec resolves to (status "stale"), re-open it.
   */
  async checkView() {
    const id = this.client.view?.id;
    if (!id) return;
    const r = await fetch(`/api/v1/views/${id}`, { cache: "no-store" }).catch(() => undefined);
    if (!r || id !== this.client.view.id) return;
    if (!r.ok) return void this.client.noteFailure(r);
    const v = (await r.json().catch(() => undefined)) as { status?: string } | undefined;
    if (v?.status === "stale" && !this.isRange) await this.recoverView();
  }

  get isRange() {
    return this.client.isRange;
  }

  /** Open any view spec (agent navigation: view.requested). */
  async openView(spec: unknown) {
    if (await this.switchView(spec)) this.home(600);
  }

  private drawMoveGhost(r?: Rect) {
    const g = this.moveGhost.clear();
    if (r) {
      const px = 1 / this.cam.zoom;
      g.rect(r.x, r.y, r.w, r.h).fill({ color: this.look.theme.accent, alpha: 0.08 }).stroke({ width: 2 * px, color: this.look.theme.accent, alpha: 0.9 });
    }
    this.dirty = true;
  }

  /** Called with an error message when a move or pin is refused (stale layout, server down). */
  onLayoutError?: (msg: string) => void;

  /**
   * Move a window (by WindowKey) or cluster (`cluster:<id>`) to (x, y) and pin or unpin it there
   * (PATCH /views/:id/layout), then reload the layout in place. A stale layout reloads and reports.
   */
  async moveTo(key: string, x: number, y: number, pinned: boolean) {
    const r = await fetch(`/api/v1/views/${this.client.view.id}/layout`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ moves: [{ key, x: Math.round(x), y: Math.round(y), pinned }], baseLayoutVersion: this.client.view.layoutVersion }),
    }).catch(() => undefined);
    this.drawMoveGhost(undefined);
    if (r && !r.ok) void this.client.noteFailure(r);
    if (!r?.ok) {
      const msg = r?.status === 409 ? "The layout changed meanwhile; reloaded it, try again." : `Could not move it (${r?.status ?? "offline"}).`;
      if (r?.status === 409) await this.reloadLayout(key);
      this.onLayoutError?.(msg);
      return false;
    }
    await this.reloadLayout(key);
    return true;
  }

  /** Re-open the current view after a layout change (moved windows can land in other tiles), keeping the camera and re-selecting `key`. */
  async reloadLayout(key?: string) {
    const cam = this.cam;
    const [x, y, z] = [cam.x, cam.y, cam.zoom];
    if (!(await this.switchView(this.client.view.spec))) return;
    cam.animateTo(x, y, z, 0);
    if (!key) return;
    if (key.startsWith("cluster:")) {
      const c = this.client.boot.clusters.find((c) => c.id === key.slice(8));
      if (c) this.select({ type: "cluster", c });
      return;
    }
    // The window's tile loads after the camera settles; select it once it is there, unless
    // something else was selected meanwhile.
    const seq = this.selectSeq;
    for (let i = 0; i < 40 && this.selectSeq === seq; i++) {
      const w = [...this.client.windows.values()].find((w) => w.key === key);
      if (w) return this.select({ type: "window", w });
      await new Promise((r) => setTimeout(r, 100));
    }
  }

  /** Re-read the bootstrap (cluster labels and summaries) and redraw the backdrop and HUD. */
  async reloadBootstrap() {
    const r = await fetch(`/api/v1/views/${this.client.view.id}/bootstrap`);
    if (!r.ok) return void this.client.noteFailure(r);
    const b = await r.json();
    for (const c of this.client.boot.clusters) {
      const n = b.clusters?.find((x: { id: string }) => x.id === c.id);
      if (n) Object.assign(c, { label: n.label, labelSource: n.labelSource, summary: n.summary });
    }
    this.backdrop.build(this.client.boot, this.look.theme);
    this.spokes.setBoot(this.client.boot);
    this.spokesDirty = true;
    this.tessellate(this.backdrop.back);
    this.tessellate(this.backdrop.front);
    this.backdrop.setTier(tierIndex(this.tier));
    this.hud.build(this.client.boot, this.look.theme);
    this.dirty = true;
  }

  /** Swap the whole view: drop every chunk, edge page, text mesh and focus, then rebuild. */
  private switchSeq = 0;

  /** View switches in flight (stats().pendingSwitch: tests wait for a switch to finish). */
  private switching = 0;

  private async switchView(spec: unknown) {
    this.switching++;
    try {
      return await this.switchViewNow(this.withLayout(spec));
    } finally {
      this.switching--;
    }
  }

  private async switchViewNow(spec: unknown) {
    const seq = ++this.switchSeq;
    this.releaseGather(true);
    this.select(undefined);
    this.hover = undefined;
    if (!(await this.client.switchView(spec, () => seq === this.switchSeq))) return false;
    for (const c of this.chunks.values()) c.destroy();
    this.chunks.clear();
    this.visible.clear();
    this.visibleEdges.clear();
    for (const { c } of this.code.values()) dispose(c);
    this.code.clear();
    this.codeQueue.clear();
    this.fading = [];
    this.expanded.clear();
    this.fragments = undefined;
    this.fragDirty = true;
    this.staleFar.clear();
    this.refreshPending.clear();
    const vs = this.client.view.spec as ViewSpec;
    this.look = {
      ...this.look,
      contextDisplay: vs.mode === "range" ? vs.options?.contextDisplay : undefined,
      ownerSlot: vs.mode === "range" ? (o) => this.client.ownerSlot(o) : undefined,
    };
    this.longEdges.reset();
    this.bundles.setData(this.client.boot);
    this.setTheme(this.look.theme);
    // Pairs and cluster ids belong to the view: drop them, and fetch the new view's if the lens is on.
    this.setCochange(undefined);
    this.cochangeFor = undefined;
    if (this.look.lens === "cochange") void this.loadCochange();
    setStructure(undefined);
    this.structureView = undefined;
    this.onStructure?.(undefined);
    if (isTreeLens(this.look.lens)) void this.loadStructure();
    this.threadsDirty = true;
    this.lastData = 0;
    this.camVersion = -1;
    if (vs.mode === "snapshot") this.lastSnapshot = vs;
    this.onViewChange?.(this.client.view.id);
    return true;
  }

  /** The last snapshot spec shown: where "Snapshot" and "back to the snapshot" return from a range. */
  private lastSnapshot?: ViewSpec;

  /** Leave a range view for the snapshot it came from (`@` unless another revision was opened). */
  async backToSnapshot() {
    if (await this.switchView(this.lastSnapshot ?? { mode: "snapshot", rev: "@" })) this.home(600);
  }

  /** Called after every view switch with the new view id (the app subscribes to its events). */
  onViewChange?: (viewId: string) => void;

  /** Expand or collapse a range-view context pill, in place. */
  toggleExpanded(w: WindowHeader) {
    if (this.expanded.has(w.id)) this.expanded.delete(w.id);
    else this.expanded.add(w.id);
    const c = this.chunks.get(cellKey(Math.floor(w.rect.x / CELL_WU), Math.floor(w.rect.y / CELL_WU)));
    for (const p of ["frames", "sil", "ports"] as Part[]) c?.dirty.add(p);
    this.dirty = true;
  }

  /** Hotspots brush: only these clusters stay at full strength (undefined clears it). */
  setBrush(clusters?: ReadonlySet<string>) {
    this.look = { ...this.look, brush: clusters };
    for (const c of this.chunks.values()) {
      c.dirty.add("frames");
      c.dirty.add("edges");
    }
    this.backdrop.setLens(this.look.lens, clusters);
    this.restyleBundles();
    this.dirty = true;
  }

  /** Where a file's windows are, loaded or not (fragments route). */
  async locate(path: string): Promise<Rect[]> {
    return (await this.fragmentsOf(path))?.fragments.map((x) => x.rect) ?? [];
  }

  async fragmentsOf(path: string): Promise<FragmentsResponse | undefined> {
    const r = await fetch(`/api/v1/views/${this.client.view.id}/fragments?path=${encodeURIComponent(path)}`).catch(() => undefined);
    if (r && !r.ok) void this.client.noteFailure(r);
    if (!r?.ok) return undefined;
    return (await r.json()) as FragmentsResponse;
  }

  /**
   * ]f / [f (D14): fly to the next or previous fragment of the selected file and select it once its
   * tile has loaded. Wraps around.
   */
  async jumpFragment(dir: 1 | -1) {
    const sel = this.selected;
    if (sel?.type !== "window" || !sel.w.fragment) return;
    const f = this.fragments?.path === sel.w.path ? this.fragments : await this.fragmentsOf(sel.w.path);
    if (!f?.fragments.length) return;
    const list = [...f.fragments].sort((a, b) => a.index - b.index);
    const at = list.findIndex((x) => x.window === sel.w.id);
    const next = list[(Math.max(0, at) + dir + list.length) % list.length];
    const [x, y, z] = this.frameFor(grow(next.rect, 80));
    this.cam.flyAlong(x, y, Math.max(z, Math.min(this.cam.zoom, 0.6)));
    const seq = this.selectSeq;
    for (let i = 0; i < 40 && this.selectSeq === seq; i++) {
      const w = this.client.windows.get(next.window);
      if (w) return this.select({ type: "window", w });
      await new Promise((r) => setTimeout(r, 100));
    }
  }

  /**
   * Dotted links between fragments of the selected file that ended up apart (D14), sized for the
   * current tier. Few quads: one dashed curve per consecutive pair.
   */
  private buildFragLinks() {
    for (const ch of this.fragLayer.removeChildren()) dispose(ch);
    this.fragLinks = 0;
    const f = this.fragments;
    if (!f || f.fragments.length < 2 || this.look.selectedPath !== f.path) return;
    const px = 1 / this.look.strokeZoom;
    const color = CHIP_HI[this.client.chipOf({ path: f.path, fragment: { chip: f.chip } } as WindowHeader)] ?? this.look.theme.accent;
    const rb = new RectBatch(256);
    const list = [...f.fragments].sort((a, b) => a.index - b.index);
    for (let i = 1; i < list.length; i++) {
      const a = list[i - 1].rect;
      const b = list[i].rect;
      const gap = Math.max(b.x - (a.x + a.w), a.x - (b.x + b.w), b.y - (a.y + a.h), a.y - (b.y + b.h));
      if (gap < 120) continue;
      const pts = curve(a.x + a.w, a.y + Math.min(a.h / 2, 200), b.x, b.y + Math.min(b.h / 2, 200), 60);
      dashedPolyline(rb, pts, 2 * px, 5 * px, 2.2 * px, color, 0.95);
      this.fragLinks++;
    }
    rb.build(this.fragLayer);
  }

  /** The file-chip palettes (wire.ts FILE_CHIP_SLOTS), at rest and for the selected file (e2e checks). */
  readonly chipPalette = { rest: CHIP_REST, hi: CHIP_HI };

  /** Side of an L2 district glyph on screen. */
  static readonly GLYPH_PX = GLYPH_PX;

  /** The world rects of the cluster labels (backdrop) of the clusters overlapping `region`. */
  clusterLabelRects(region: Rect): Array<Rect & { label: string }> {
    const font = sansFont();
    const L = CLUSTER_LABEL;
    return this.client.boot.clusters.filter((c) => intersects(c.rect, region)).map((c) => ({ ...clusterLabelRect(c, textWidth(font, fit(font, c.label, L.size, c.rect.w - L.inset), L.size)), label: c.label }));
  }

  /** The L2 district glyphs of the windows around the view, at GLYPH_PX on screen (one mesh). */
  private buildGlyphs() {
    this.glyphDirty = false;
    const cam = this.cam;
    const z = cam.zoom;
    const px = 1 / z;
    const v = cam.view();
    const region = grow(v, v.w * 0.5, v.h * 0.5);
    this.glyphBuilt = { zoom: z, region };
    const t = this.look.theme;
    const G = Engine.GLYPH_PX * px;
    this.glyphStats = { count: 0, px: 0, moved: 0 };
    this.glyphRects = [];
    const cell = G / 5;
    const ws = this.client.windowsIn(region);
    const rb = new RectBatch(ws.length * 6);
    let count = 0;
    const drawn = (w: WindowHeader) => !(isMinimized(w, this.look) || isHiddenContext(w, this.look) || this.look.hiddenLanes?.has(w.lane));
    // What a glyph must not cover: other windows and the cluster labels (backdrop) around the view.
    const labels = this.clusterLabelRects(region);
    for (const w of ws) {
      if (!drawn(w)) continue;
      const r = w.rect;
      if (r.w * z < G * z * 2) continue;
      const blocked = (s: Rect) => labels.some((l) => intersects(l, s)) || this.client.windowsIn(s).some((o) => o !== w && drawn(o) && intersects(o.rect, s));
      const { x: gx, y: gy, where } = glyphSpot(r, G, px, blocked);
      if (where !== "corner") this.glyphStats.moved++;
      if (this.glyphRects.length < 2000) this.glyphRects.push({ id: w.id, x: gx, y: gy, w: G, h: G });
      // A small card: border, plate, the cell's row and column, and the cell lit in its heat colour.
      const b = GLYPH_BORDER_PX * px;
      rb.round(gx - b, gy - b, G + 2 * b, G + 2 * b, 3 * px, t.line);
      rb.round(gx - px, gy - px, G + 2 * px, G + 2 * px, 2.5 * px, t.header);
      const col = 4 - w.heat.grade;
      const row = 4 - w.conn.grade;
      rb.add(gx, gy + row * cell, G, cell, mix(t.header, t.line, 0.55));
      rb.add(gx + col * cell, gy, cell, G, mix(t.header, t.line, 0.55));
      rb.add(gx + col * cell, gy + row * cell, cell, cell, t.heat[w.heat.grade]);
      count++;
    }
    rb.build(this.glyphLayer);
    this.glyphStats.count = count;
    this.glyphStats.px = G * z;
  }

  /**
   * Fly to a window (by id, or the window of `path` holding `line`), select it once its tile has
   * loaded, and return it.
   */
  async goTo(target: { window?: string; path?: string; line?: number }): Promise<WindowHeader | undefined> {
    const holds = (w: WindowHeader) => target.line === undefined || (target.line >= w.lines.start && target.line <= w.lines.end);
    const find = (exact = false) => {
      if (target.window) return this.client.windows.get(target.window);
      const ws = target.path ? this.client.windowsByPath.get(target.path) : undefined;
      return ws?.find(holds) ?? (exact ? undefined : ws?.[0]);
    };
    let w = find(true);
    if (!w && target.path) {
      const rects = await this.locate(target.path);
      if (rects.length) {
        // Fly to the fragment holding the line: its header comes from the base tiles under the
        // fragments, before the camera gets there and loads them for real.
        const peeked = target.line === undefined ? [] : await this.client.peekHeaders(rects);
        const hit = peeked.find((p) => p.path === target.path && holds(p));
        const [x, y, z] = this.frameFor(grow(hit?.rect ?? rects[0], 80));
        this.cam.flyAlong(x, y, z);
        for (let i = 0; i < 40 && !(w = find(true)); i++) await new Promise((r) => setTimeout(r, 100));
        w ??= find();
      }
    } else if (!w) {
      w = find();
    }
    if (!w && target.window) {
      // Unknown window id and no path: nothing to fly to.
      return undefined;
    }
    if (w) {
      // A target in a closed lane (docs start collapsed, DESIGN §4.3) opens that lane.
      if (this.look.hiddenLanes?.has(w.lane)) {
        const open = new Set(this.look.hiddenLanes);
        open.delete(w.lane);
        this.setHiddenLanes(open);
        this.onHiddenLanes?.(open);
      }
      // With a line, centre that line (tall windows would otherwise open at their top).
      const target_ = target.line !== undefined && w.rect.h > 900
        ? { x: w.rect.x - 80, y: w.rect.y + HEADER_WU + (target.line - w.lines.start) * LINE_WU - 400, w: w.rect.w + 160, h: 800 }
        : grow(w.rect, 80);
      const [x, y, z] = this.frameFor(target_);
      this.cam.flyAlong(x, y, z);
      this.select({ type: "window", w });
    }
    return w;
  }

  /** Select a target (or nothing): its links are unbundled and everything else dims. */
  /** Bumped on every selection change (a pending re-select backs off when it moves on). */
  private selectSeq = 0;

  select(t?: Target) {
    if (targetKey(t) === targetKey(this.selected)) return;
    this.selected = t;
    this.selectSeq++;
    this.setSelectedPath(t?.type === "window" && t.w.fragment ? t.w.path : undefined);
    this.overlayDirty = true;
    this.noteCochangeFocus();
    this.onSelectCb?.(t);
  }

  /** Subscribe to camera changes (DOM overlays pinned to world points follow with it). */
  onView(cb: () => void): () => void {
    this.viewListeners.add(cb);
    return () => void this.viewListeners.delete(cb);
  }

  /** The file line under world y inside window `w`. */
  /** Show (or clear) the lines a comment is anchored to; editable ranges get drag handles. */
  setCommentRange(r: LineRange | undefined) {
    this.rangeMark.set(r);
    this.rangeDirty = true;
    this.dirty = true;
  }

  /**
   * Point at a line of a window from outside the canvas (a side-panel row under the pointer): a
   * band in `color` over the line, and `edge` drawn as hovered. `undefined` clears both.
   */
  peek(p?: { w?: WindowHeader; line?: number; color?: number; edge?: Edge }) {
    this.peekMark.set(p?.w && p.line !== undefined ? { w: p.w, start: p.line, end: p.line, editable: false, color: p.color } : undefined);
    this.peekDirty = true;
    const t: Target | undefined = p?.edge ? { type: "edge", e: p.edge } : undefined;
    if (targetKey(t) !== targetKey(this.hover)) {
      this.hover = t;
      this.overlayDirty = true;
    }
    this.dirty = true;
  }

  get commentRange(): LineRange | undefined {
    return this.rangeMark.range;
  }

  lineAt(w: WindowHeader, y: number) {
    return Math.max(w.lines.start, Math.min(w.lines.end, w.lines.start + Math.floor((y - w.rect.y - HEADER_WU) / LINE_WU)));
  }

  /** An agent looked at or changed something: show it on the canvas. */
  attention(a: Attention) {
    this.presence.add(a);
    this.presenceDirty = true;
  }

  /**
   * Widen a window's lines (POST …/expand). In a snapshot view the server makes it a layout change:
   * the window keeps its top and pushes what it would cover down (its column, then the clusters
   * below), and the response carries the new layoutVersion, so the layout reloads in place. In a
   * range view (and on servers without that) the window grows over its neighbours, which stay where
   * they are; it moves to the end of its chunk so it draws on top.
   */
  async expandWindow(w: WindowHeader, req: { before?: number | "file"; after?: number | "file" }) {
    const r = await fetch(`/api/v1/views/${this.client.view.id}/windows/${w.id}/expand`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ ...req, lines: { start: w.lines.start, end: w.lines.end } }),
    });
    await this.resized(w, r);
  }

  /** Take an expanded window back to its size before (POST …/collapse, the reverse of {@link expandWindow}). */
  async collapseWindow(w: WindowHeader) {
    const r = await fetch(`/api/v1/views/${this.client.view.id}/windows/${w.id}/collapse`, { method: "POST" });
    await this.resized(w, r);
  }

  /** Apply an expand or collapse response: reload the layout (snapshot views) or swap the header in place. */
  private async resized(w: WindowHeader, r: Response) {
    if (!r.ok) return void this.client.noteFailure(r);
    const nw = (await r.json()) as WindowHeader & { layoutVersion?: number };
    if (nw.layoutVersion !== undefined) {
      const sel = this.selected?.type === "window" && this.selected.w.id === w.id;
      await this.reloadLayout(sel ? nw.key : undefined);
      return;
    }
    const key = cellKey(Math.floor(w.rect.x / CELL_WU), Math.floor(w.rect.y / CELL_WU));
    this.client.replaceWindow(w, nw);
    const e = this.code.get(w.id);
    if (e) {
      dispose(e.c);
      this.code.delete(w.id);
    }
    const chunk = this.chunks.get(key);
    if (chunk) chunk.setWindows(this.client.cells.get(key) ?? []);
    this.updateData(performance.now());
    if (this.selected?.type === "window" && this.selected.w.id === w.id) this.selected = { type: "window", w: nw };
    this.threadsDirty = true;
    this.overlayDirty = true;
    // What is under the pointer changed with the window; pick again.
    if (this.pointer) this.pointer.moved = true;
    this.dirty = true;
  }

  /** A conflict was resolved: clear the header badges of that file's windows. */
  clearConflicts(path: string) {
    for (const w of this.client.windowsByPath.get(path) ?? []) {
      w.counts.conflicts = 0;
      this.chunks.get(cellKey(Math.floor(w.rect.x / CELL_WU), Math.floor(w.rect.y / CELL_WU)))?.dirty.add("frames");
    }
    this.threadsDirty = true;
    this.dirty = true;
  }

  /**
   * edit.updated from anyone (an agent's amend, an undo): reload the windows of the files it wrote.
   * Every window of a snapshot shares its change, so files, not changes, say what moved.
   */
  refreshAfterEdit(edit: EditResult) {
    if (edit.status !== "applied" && edit.status !== "undone") return;
    const paths = new Set((edit.files ?? []).map((f) => f.path));
    const ids: string[] = [];
    for (const p of paths) for (const w of this.client.windowsByPath.get(p) ?? []) ids.push(w.id);
    if (ids.length) return this.refreshWindows(ids);
  }

  /** After this client changed a file: new headers (blob ids), then text and meshes. */
  refreshFile(path: string) {
    return this.refreshWindows((this.client.windowsByPath.get(path) ?? []).map((w) => w.id));
  }

  /**
   * view.invalidated with window ids: fetch their new headers (new blob ids after an op), then
   * reload those files' text, outlines and code meshes. Calls within a few milliseconds are merged
   * (an edit sends edit.updated and view.invalidated for the same windows), and windows far from
   * the view are only marked: their headers are refetched when the camera comes near (F2:
   * invalidation refetches only what changed, and only what is needed).
   */
  refreshWindows(ids: string[]): Promise<void> {
    for (const id of ids) this.refreshPending.add(id);
    return (this.refreshing ??= new Promise<void>((res) =>
      setTimeout(() => {
        this.refreshing = undefined;
        this.flushRefresh().then(res, res);
      }, 30),
    ));
  }
  private refreshPending = new Set<string>();
  private refreshing?: Promise<void>;
  /** Invalidated windows away from the view, refreshed when the camera comes near them. */
  private staleFar = new Set<string>();

  private async flushRefresh() {
    const v = this.cam.view();
    const near = grow(v, v.w, v.h);
    const ids: string[] = [];
    for (const id of this.refreshPending) {
      const w = this.client.windows.get(id);
      if (!w) continue;
      if (intersects(w.rect, near)) ids.push(id);
      else this.staleFar.add(id);
    }
    this.refreshPending.clear();
    if (!ids.length) return;
    const paths = new Set<string>();
    for (const id of ids) {
      this.staleFar.delete(id);
      const w = this.client.windows.get(id);
      if (w) paths.add(w.path);
    }
    const changed = await this.client.reloadHeaders(ids);
    for (const w of changed) {
      const key = cellKey(Math.floor(w.rect.x / CELL_WU), Math.floor(w.rect.y / CELL_WU));
      this.chunks.get(key)?.setWindows(this.client.cells.get(key) ?? []);
    }
    for (const p of paths) this.refreshPath(p);
  }

  /** view.invalidated with tiles ("z/x/y"): refresh the loaded windows in them. */
  refreshTiles(tiles: string[]) {
    const ids: string[] = [];
    for (const k of tiles) {
      const [z, x, y] = k.split("/").map(Number);
      const size = TILE_BASE_WU * (1 << z);
      for (const w of this.client.windowsIn({ x: x * size, y: y * size, w: size, h: size })) ids.push(w.id);
    }
    if (ids.length) this.refreshWindows(ids);
  }

  /** Invalidated windows the camera has come near: refresh them now. */
  private refreshNear() {
    if (!this.staleFar.size) return;
    const v = this.cam.view();
    const near = grow(v, v.w, v.h);
    const ids: string[] = [];
    for (const id of this.staleFar) {
      const w = this.client.windows.get(id);
      if (!w) this.staleFar.delete(id);
      else if (intersects(w.rect, near)) ids.push(id);
    }
    if (ids.length) this.refreshWindows(ids);
  }

  /** After an edit: reload a file's text, outlines and code meshes. */
  refreshPath(path: string) {
    for (const w of this.client.forget(path)) {
      const e = this.code.get(w.id);
      if (e) {
        dispose(e.c);
        this.code.delete(w.id);
      }
      const c = this.chunks.get(cellKey(Math.floor(w.rect.x / CELL_WU), Math.floor(w.rect.y / CELL_WU)));
      c?.dirty.add("sil");
      c?.dirty.add("ports");
    }
    this.threadsDirty = true;
    this.updateData(performance.now());
    this.dirty = true;
  }

  get theme() {
    return this.look.theme;
  }

  /** Highways leaving or entering a cluster, heaviest first. */
  highwaysOf(clusterId: string) {
    return this.bundles.highways
      .filter((h) => h.count && (h.from.id === clusterId || h.to.id === clusterId))
      .sort((a, b) => b.count - a.count);
  }

  /** A window's links plus its hub spokes. */
  private withSpokes(edges: Edge[] | undefined, windowId: string): Edge[] {
    const s = this.spokes.byWindow.get(windowId);
    return s ? [...(edges ?? []), ...s] : (edges ?? []);
  }

  /** Individual links behind a target. */
  edgesOf(t: Target): Edge[] {
    const c = this.client;
    switch (t.type) {
      case "window":
        return this.withSpokes(c.edgesByWindow.get(t.w.id), t.w.id);
      case "cluster":
        return c.clusterEdges(t.c.id);
      case "highway":
        return c.pairEdges(t.h.key);
      case "edge":
        return [t.e];
      case "hub":
        return this.spokes.byHub.get(t.h.id) ?? [];
      case "thread":
      case "conflict":
      case "expand":
        return [];
      case "port": {
        // The links leaving (or entering) the window at the pill's line.
        // A pill merged from neighbouring lines (N14) carries all of their links.
        const { window: w, side, lines } = t.p;
        const lineOf = (y: number) => w.lines.start + Math.floor((y - w.rect.y - HEADER_WU) / LINE_WU);
        return this.withSpokes(c.edgesByWindow.get(w.id), w.id).filter((e) =>
          side === "out" ? e.from === w.id && lines.includes(lineOf(e.ay)) : e.to === w.id && lines.includes(lineOf(e.by)),
        );
      }
    }
  }

  /** Fly along a link to its far end, seen from `fromWindow`, and select what is there. */
  follow(e: Edge, fromWindow?: string) {
    const toFar = fromWindow ? e.from === fromWindow : true;
    const id = toFar ? e.to : e.from;
    const [x, y] = toFar ? [e.bx, e.by] : [e.ax, e.ay];
    const w = this.client.windows.get(id);
    // Long enough (1.5 s, arcing out and back in) to see where the link goes.
    if (w) {
      this.select({ type: "window", w });
      const [fx, fy, fz] = this.frameFor(grow(w.rect, 80));
      this.cam.flyAlong(fx, fy, Math.max(fz, Math.min(this.cam.zoom, 0.6)));
    } else {
      const z = Math.max(this.cam.zoom, 0.3);
      this.cam.flyAlong(x - this.cam.width / 2 / z, y - this.cam.height / 2 / z, z);
    }
  }

  focusTarget(t: Target) {
    switch (t.type) {
      case "window":
        return this.frameRect(grow(t.w.rect, 80), 450);
      case "cluster":
        return this.frameRect(grow(t.c.rect, 200), 450);
      case "highway":
        return this.frameRect(bounds([t.h.from.rect, t.h.to.rect]), 450);
      case "edge":
        return this.frameRect(bounds([{ x: t.e.ax, y: t.e.ay, w: 1, h: 1 }, { x: t.e.bx, y: t.e.by, w: 1, h: 1 }]), 450);
      case "hub":
        if (t.h.rect) return this.frameRect(grow(t.h.rect, 2000), 450);
        return;
      case "port":
        return this.frameRect(grow(t.p.window.rect, 80), 450);
      case "thread":
      case "conflict":
      case "expand":
        return this.frameRect(grow(t.h.window.rect, 80), 450);
    }
  }

  setSelectedPath(path?: string) {
    if (this.look.selectedPath === path) return;
    this.look = { ...this.look, selectedPath: path };
    for (const c of this.chunks.values()) c.dirty.add("frames");
    this.dirty = true;
    // Every fragment of the file, loaded or not, goes on the minimap (fragments route); fragments
    // that ended up apart get dotted links.
    this.hud.markFragments([], 0);
    this.fragments = undefined;
    this.fragDirty = true;
    if (path) {
      const w = this.client.windowsByPath.get(path)?.[0];
      const color = w?.fragment ? CHIP_HI[w.fragment.chip] : this.look.theme.accent;
      this.fragmentsOf(path).then((f) => {
        if (this.look.selectedPath === path && f) {
          this.fragments = f;
          this.fragDirty = true;
          this.hud.markFragments(f.fragments.map((x) => x.rect), color);
          this.dirty = true;
        }
      });
    }
  }

  /** Frame the review-first cell: code lane × Hot column × Core row (DESIGN §4.9). */
  home(ms = 450) {
    const b = this.client.boot;
    const lane = b.lanes.find((l) => l.lane === "code") ?? b.lanes[0];
    // The folder tree has no cells: its code lane.
    if (!lane.rows.length) return this.frameRect(lane.rect, ms);
    const conn = Math.max(...lane.rows.map((r) => r.conn));
    const cell = b.clusters.filter((c) => c.lane === lane.lane && c.conn === conn && c.heat === 4);
    this.frameRect(cell.length ? bounds(cell.map((c) => c.rect)) : lane.rect, ms);
  }

  /** Fit: the whole map, or in a range view only the clusters it changes (D16). */
  fit(ms = 450) {
    const changed = this.changedClusters();
    this.frameRect(changed.length ? grow(bounds(changed.map((c) => c.rect)), 400) : this.client.boot.world, ms);
  }

  /** Fly so `r` fills the canvas area not covered by the rail and column header. */
  frameRect(r: Rect, ms = 450) {
    const [x, y, z] = this.frameFor(r);
    this.cam.animateTo(x, y, z, ms);
  }

  /** Camera (x, y, zoom) that frames `r`, capped so tall windows open at a readable zoom. */
  private frameFor(r: Rect): [number, number, number] {
    const cam = this.cam;
    let z = clamp(Math.min((cam.width - RAIL_PX - 60) / r.w, (cam.height - COLHEAD_PX - 60) / r.h), 0.002, MAX_ZOOM);
    let rh = r.h;
    if (z < 0.5 && r.w * 0.5 < cam.width - RAIL_PX - 60 && r.h > r.w * 2) {
      // A tall window: read it from the top instead of shrinking it to fit.
      z = 0.6;
      rh = (cam.height - COLHEAD_PX - 60) / z;
    }
    const x = r.x - (RAIL_PX + 30) / z - (cam.width - RAIL_PX - 60 - r.w * z) / 2 / z;
    const y = r.y - (COLHEAD_PX + 30) / z - (cam.height - COLHEAD_PX - 60 - rh * z) / 2 / z;
    return [x, y, z];
  }

  windowAt(sx: number, sy: number): WindowHeader | undefined {
    if (this.tier === "L0") return undefined;
    const [wx, wy] = this.cam.toWorld(sx, sy);
    return this.client.windowsIn({ x: wx, y: wy, w: 1, h: 1 })[0];
  }

  private chunk(key: string): Chunk {
    let c = this.chunks.get(key);
    if (!c) {
      const [cx, cy] = key.split(",").map(Number);
      c = new Chunk(key, { x: cx * CELL_WU, y: cy * CELL_WU, w: CELL_WU, h: CELL_WU });
      this.chunks.set(key, c);
      // Roots join the scene only while in view (onCamera): Pixi walks every attached render
      // group each frame, visible or not, and that walk was the top CPU and garbage cost.
      this.applyPartVisibility(c);
      this.camVersion = -1;
    }
    return c;
  }

  private applyPartVisibility(c: Chunk) {
    const t = tierIndex(this.tier);
    c.parts.code.visible = t >= 3;
    c.parts.sil.visible = t >= 2;
    c.parts.ports.visible = t >= 2;
    c.parts.titles.visible = this.labelMode === "titles";
    c.parts.heads.visible = this.labelMode === "heads";
  }

  private neededParts(): Part[] {
    const t = tierIndex(this.tier);
    if (t === 0) return [];
    const parts: Part[] = ["frames", "edges"];
    if (this.labelMode === "titles") parts.push("titles");
    if (this.labelMode === "heads") parts.push("heads");
    if (t >= 2) parts.push("sil", "ports");
    return parts;
  }

  /** Frames whose work took > 20 ms, with where the time went. */
  slowFrames: Array<{ at: number; total: number; phases: Record<string, number>; builds: string[]; inBench: boolean }> = [];
  private phases: Record<string, number> = {};
  private buildsThisFrame: string[] = [];
  /** Main-thread time of the last frame (camera, data, builds, render submit), for benchmarks. */
  lastFrameWork = 0;
  private mark(name: string, t0: number) {
    const t = performance.now();
    this.phases[name] = (this.phases[name] ?? 0) + t - t0;
    return t;
  }

  private frame = (now: number) => {
    this.raf = requestAnimationFrame(this.frame);
    const tStart = performance.now();
    this.phases = {};
    this.buildsThisFrame = [];
    this.frameWork(now);
    const total = performance.now() - tStart;
    this.lastFrameWork = total;
    if (total > 20) {
      this.slowFrames.push({ at: now, total, phases: this.phases, builds: [`${this.tier}@${this.cam.zoom.toFixed(3)} n=${this.buildsThisFrame.length}`, ...this.buildsThisFrame.sort((a, b) => parseFloat(b.split(" ").pop()!) - parseFloat(a.split(" ").pop()!))], inBench: !!this.beforeFrame });
      if (this.slowFrames.length > 50) this.slowFrames.shift();
    }
  };

  private frameWork(now: number) {
    const dt = Math.min(100, now - this.lastFrame);
    this.lastFrame = now;

    this.beforeFrame?.(now, dt);
    this.applyWheel(dt);
    this.cam.step(now, dt);
    const cam = this.cam;
    const moving = cam.moving || now - cam.lastMotion < SETTLE_MS || !!this.wheel;
    // Frame stats only while navigating: idle frames don't render, and browsers slow rAF then.
    if (moving) {
      this.frameTimes.push(dt);
      if (this.frameTimes.length > 240) this.frameTimes.shift();
    }

    let t = performance.now();
    if (cam.version !== this.camVersion) {
      this.camVersion = cam.version;
      this.onCamera(now);
      this.dirty = true;
      t = this.mark("camera", t);
    } else if (!moving && now - this.lastData > DATA_THROTTLE_MS && this.lastData < cam.lastMotion) {
      // Settled: one last data update at the final position.
      this.updateData(now);
    }

    if (this.outlineCells.size) this.applyOutlines();
    const budget = moving ? BUDGET_MOVING_MS : BUDGET_IDLE_MS;
    if (this.runBuilds(now, budget)) this.dirty = true;

    if (this.tier === "L3") {
      t = performance.now();
      if (this.buildCode(now, moving ? 3 : 8)) this.dirty = true;
      t = this.mark("text", t);
    }
    // Galaxy labels stay ~13 px on screen: re-lay them out once the camera settles at a new L0 zoom.
    if (this.tier === "L0" && !moving && Math.abs(Math.log(cam.zoom / (this.backdrop.labelZoom || 1))) > 0.25) {
      t = performance.now();
      this.backdrop.buildGalaxyLabels(cam.zoom);
      this.backdrop.setLensZoom(cam.zoom);
      this.dirty = true;
      t = this.mark("galaxy-labels", t);
    }
    if (!moving && now - this.lastRelease > 1000) this.releaseChunks(now);
    // Index lists eviction left stale are compacted in idle frames.
    if (!moving) this.client.sweep(2);
    if (this.fragDirty) {
      this.fragDirty = false;
      this.buildFragLinks();
      this.tessellate(this.fragLayer);
      this.dirty = true;
    }
    if (this.tier === "L2" && !moving) {
      const b = this.glyphBuilt;
      const v = cam.view();
      const inside = b && v.x >= b.region.x && v.y >= b.region.y && v.x + v.w <= b.region.x + b.region.w && v.y + v.h <= b.region.y + b.region.h;
      if (this.glyphDirty || !b || !inside || Math.abs(Math.log(cam.zoom / b.zoom)) > 0.15) {
        t = performance.now();
        this.buildGlyphs();
        this.dirty = true;
        t = this.mark("glyphs", t);
      }
    } else if (this.tier !== "L2" && this.glyphBuilt) {
      for (const ch of this.glyphLayer.removeChildren()) dispose(ch);
      this.glyphBuilt = undefined;
      this.glyphStats = { count: 0, px: 0, moved: 0 };
    }
    if (!moving && this.cochangeFor && this.look.lens === "cochange" && this.builtCochangeZoom && Math.abs(Math.log(cam.zoom / this.builtCochangeZoom)) > 0.3) this.cochangeDirty = true;
    if (this.cochangeDirty && !moving) {
      this.cochangeDirty = false;
      this.buildCochange();
      this.dirty = true;
    }
    if (!moving && (this.spokesDirty || this.spokes.builtFor !== 1 / this.look.strokeZoom)) {
      // Pages are built in runBuilds, within the frame budget.
      this.spokesDirty = false;
      this.spokes.start(this.look.theme, 1 / this.look.strokeZoom, this.look.hidden);
    }
    if (this.tier === "L0" && this.spokes.building) {
      // runBuilds only steps highways at L0; spokes show from L1, so finish them in idle frames.
      t = performance.now();
      const g = this.spokes.step();
      if (g) this.tessellate(g);
      t = this.mark("spokes", t);
    }
    if (this.peekDirty || (this.peekMark.range && this.peekMark.builtFor !== 1 / this.look.strokeZoom)) {
      this.peekDirty = false;
      this.peekMark.build(this.look.theme, 1 / this.look.strokeZoom);
      this.dirty = true;
    }
    if (this.rangeDirty || (this.rangeMark.range && this.rangeMark.builtFor !== 1 / this.look.strokeZoom)) {
      this.rangeDirty = false;
      this.rangeMark.build(this.look.theme, 1 / this.look.strokeZoom);
      this.dirty = true;
    }
    if (this.threadsDirty && !moving && this.tier !== "L0" && this.tier !== "L1") {
      t = performance.now();
      this.threadsDirty = false;
      this.threadMarks.build(this.review, this.client.windowsByPath, this.look.theme, 1 / this.look.strokeZoom);
      this.dirty = true;
      t = this.mark("threads", t);
    }
    // Agent presence fades out; redraw a few times a second while it does.
    if (this.presenceDirty || (this.presenceLast && now - this.presenceLast > 250)) {
      this.presenceDirty = false;
      const fading = this.presence.build(Date.now(), (id) => this.client.windows.get(id), this.look.theme, 1 / cam.zoom);
      this.presenceLast = fading ? now : 0;
      this.dirty = true;
    }
    if (this.pointer?.moved && !moving) {
      t = performance.now();
      this.pointer.moved = false;
      this.updateHover();
      t = this.mark("pick", t);
    }
    this.stepGather(now, moving);
    // The overlay is sized for one zoom; refresh it once the camera settles somewhere else.
    // The overlay is sized for one zoom and dims the windows around one view; refresh it once the
    // camera settles somewhere else.
    const v = cam.view();
    const outside = !!this.overlayRegion && !(v.x >= this.overlayRegion.x && v.y >= this.overlayRegion.y && v.x + v.w <= this.overlayRegion.x + this.overlayRegion.w && v.y + v.h <= this.overlayRegion.y + this.overlayRegion.h);
    if (this.overlayDirty || (this.focusActive && !moving && (outside || Math.abs(Math.log(cam.zoom / this.overlay.builtZoom)) > 0.2))) {
      this.overlayRegion = grow(v, v.w, v.h);
      const region = this.overlayRegion;
      t = performance.now();
      this.overlayDirty = false;
      const active = this.overlay.draw({
        theme: this.look.theme,
        zoom: cam.zoom,
        hidden: this.look.hidden,
        selected: this.selected,
        hover: this.hover,
        edgesOf: (x) => this.edgesOf(x),
        windowOf: (id) => this.client.windows.get(id),
        highwaysOf: (id) => this.highwaysOf(id),
        windowsNear: () => this.client.windowsIn(region),
        selectedPath: this.look.selectedPath,
        galaxy: tierIndex(this.tier) === 0 ? () => this.client.boot.clusters.filter((c) => intersects(c.rect, region)) : undefined,
        quietHover: this.look.lens === "cochange",
        flowColors: this.look.lens === "connectedness" ? { into: this.look.theme.accent, out: this.look.theme.consumer } : undefined,
      });
      if (active !== this.focusActive) {
        this.focusActive = active;
        // Focus: everything that is not the selection steps back.
        this.restyleBundles();
        this.edgeLayer.alpha = active ? 0.2 : 1;
        this.longEdges.root.alpha = active ? 0.12 : 0.6;
      }
      this.dirty = true;
      t = this.mark("overlay", t);
    }
    if (this.fading.length) {
      // Text fades in over its silhouette instead of popping.
      this.fading = this.fading.filter((f) => {
        f.c.alpha = Math.min(1, (now - f.born) / 150);
        return f.c.alpha < 1 && !f.c.destroyed;
      });
      this.dirty = true;
    }

    if (this.dirty) {
      t = performance.now();
      flushPalette();
      this.app.render();
      this.dirty = false;
      t = this.mark("render", t);
    }
    if (now - this.lastStats > 250) {
      this.lastStats = now;
      this.onStats?.(this.stats());
    }
  };

  private onCamera(now: number) {
    const cam = this.cam;
    const z = cam.zoom;
    for (const w of [this.worldBack, this.worldFront]) {
      w.scale.set(z);
      w.position.set(-cam.x * z, -cam.y * z);
    }
    this.hud.update(cam);
    for (const cb of this.viewListeners) cb();
    // The tooltip describes what was under the pointer; re-pick once the camera settles.
    this.tooltip.hide();
    if (this.pointer) this.pointer.moved = true;

    if (cam.tier !== this.tier) {
      this.tier = cam.tier;
      const t = tierIndex(this.tier);
      this.look = { ...this.look, strokeZoom: STROKE_ZOOM[this.tier], tierIndex: t };
      this.backdrop.setTier(t);
      // The tray is sized for the tier it opened at.
      this.releaseGather(true);
      // Borders, tints, link widths, arrows and port pills are all sized for the tier.
      for (const c of this.chunks.values()) for (const p of ["frames", "edges", "ports"] as Part[]) c.dirty.add(p);
      this.restyleBundles();
      // Focus dims windows from L1 on and whole clusters at L0.
      if (this.selected || this.hover) this.overlayDirty = true;
      this.threadsDirty = true;
      this.cochangeDirty = true;
      this.fragDirty = true;
    }
    this.fragLayer.visible = tierIndex(this.tier) >= 1;
    this.threadMarks.root.visible = tierIndex(this.tier) >= 2;
    this.rangeMark.root.visible = tierIndex(this.tier) >= 2;
    this.peekMark.root.visible = tierIndex(this.tier) >= 2;
    this.spokes.root.visible = tierIndex(this.tier) >= 1;
    // Label mode with its own hysteresis, so labels never flicker at a boundary.
    const t = tierIndex(this.tier);
    const mode: LabelMode = t === 0 ? "none" : z < 0.22 ? "titles" : z > 0.26 ? "heads" : this.labelMode === "none" ? "titles" : this.labelMode;
    if (mode !== this.labelMode || t !== this.partsTier) {
      this.labelMode = mode;
      this.partsTier = t;
      for (const c of this.chunks.values()) this.applyPartVisibility(c);
    }
    this.longEdges.root.visible = t >= 1;

    // Culling at chunk granularity: windows and short edges by their own bounds.
    const view = grow(cam.view(), 200 / z);
    const show = t >= 1;
    const next = new Set<Chunk>();
    const nextEdges = new Set<Chunk>();
    if (show) {
      for (const c of this.chunks.values()) {
        if (c.windows.length && intersects(c.bounds, view)) next.add(c);
        if (c.edges.length && intersects(c.edgeBounds, view)) nextEdges.add(c);
      }
    }
    for (const c of next) c.seen = now;
    for (const c of nextEdges) c.seen = now;
    for (const c of this.visible) if (!next.has(c)) this.chunkLayer.removeChild(c.root);
    for (const c of next) if (!this.visible.has(c)) this.chunkLayer.addChild(c.root);
    for (const c of this.visibleEdges) if (!nextEdges.has(c)) this.edgeLayer.removeChild(c.edgeRoot);
    for (const c of nextEdges) if (!this.visibleEdges.has(c)) this.edgeLayer.addChild(c.edgeRoot);
    this.visible = next;
    this.visibleEdges = nextEdges;

    if (now - this.lastData > DATA_THROTTLE_MS) this.updateData(now);
  }

  private updateData(now: number) {
    const t0 = performance.now();
    this.lastData = now;
    const cam = this.cam;
    const v = cam.velocityPx();
    this.client.update(cam.view(), cam.zoom, this.tier, v);
    this.refreshNear();
    this.mark("data", t0);
  }

  /** Chunks with built geometry kept at most; beyond, the least recently seen are released. */
  chunkGeometryCap = 140;
  private lastRelease = 0;

  /**
   * Bound GPU and heap memory (§5): release the geometry of the least recently seen chunks that are
   * out of view, and their L3 text. Their data stays in the client, so they rebuild on return.
   */
  private releaseChunks(now: number) {
    this.lastRelease = now;
    const built = [...this.chunks.values()].filter((c) => c.built && !this.visible.has(c) && !this.visibleEdges.has(c));
    const over = built.length + this.visible.size - this.chunkGeometryCap;
    if (over <= 0) return;
    built.sort((a, b) => a.seen - b.seen);
    const freed = new Set(built.slice(0, over));
    for (const [id, e] of this.code) {
      if (!freed.has(e.chunk)) continue;
      dispose(e.c);
      this.code.delete(id);
    }
    for (const c of freed) c.release();
  }

  /** Mark silhouettes and pills dirty in cells whose outline batches are in (OUTLINE_COALESCE_MS). */
  private applyOutlines() {
    const now = performance.now();
    for (const [k, t] of this.outlineCells) {
      if (now - t < OUTLINE_COALESCE_MS && this.client.outlinesPendingIn(k)) continue;
      this.outlineCells.delete(k);
      const c = this.chunks.get(k);
      if (!c) continue;
      c.dirty.add("sil");
      c.dirty.add("ports");
    }
  }

  /** Rebuild dirty chunk parts, nearest the view centre first, within `budget` ms. */
  private runBuilds(now: number, budget: number): boolean {
    const tSel = performance.now();
    const parts = this.neededParts();
    if (!parts.length) {
      return this.bundles.step(tSel + budget);
    }
    const view = this.cam.view();
    const pre = grow(view, view.w * 0.5, view.h * 0.5);
    const cx = view.x + view.w / 2;
    const cy = view.y + view.h / 2;
    const cands: Array<[number, Chunk]> = [];
    for (const c of this.chunks.values()) {
      if (!c.dirty.size || !(intersects(c.bounds, pre) || intersects(c.edgeBounds, pre))) continue;
      if (!parts.some((p) => c.dirty.has(p))) continue;
      const d = Math.hypot(c.bounds.x + c.bounds.w / 2 - cx, c.bounds.y + c.bounds.h / 2 - cy);
      cands.push([intersects(c.bounds, view) ? d : d + 1e9, c]);
    }
    cands.sort((a, b) => a[0] - b[0]);
    const t0 = this.mark("build-select", tSel);
    let built = false;
    const outlines = (id: string) => this.client.outlines.get(id);
    // Big parts are built in slices: a build stops at the deadline and resumes next frame.
    const deadline = t0 + budget;
    outer: for (const [, c] of cands) {
      for (const p of parts) {
        if (!c.dirty.has(p)) continue;
        const tb = performance.now();
        // Too little left to start (or resume) a part: it waits for the next frame.
        if (deadline - tb < BUILD_MIN_SLICE_MS) break outer;
        const done = c.step(p, this.look, outlines, deadline);
        if (done) {
          this.tessellate(c.parts[p]);
          built = true;
        }
        this.buildsThisFrame.push(`${p}${done ? "" : "…"}@${c.key}:${c.windows.length}w/${c.edges.length}e ${(performance.now() - tb).toFixed(1)}ms`);
        if (performance.now() > deadline) break outer;
      }
    }
    // Highways: one page per frame when budget remains (they show at every tier).
    if (performance.now() - t0 < budget && this.bundles.building) {
      const tb = performance.now();
      if (this.bundles.step(deadline)) {
        built = true;
        this.buildsThisFrame.push(`highways ${(performance.now() - tb).toFixed(1)}ms`);
      }
    }
    // Hub spokes: pages while budget remains.
    while (this.spokes.building && performance.now() - t0 < budget) {
      const tb = performance.now();
      const g = this.spokes.step();
      if (!g) break;
      this.tessellate(g);
      built = true;
      this.buildsThisFrame.push(`spokes ${(performance.now() - tb).toFixed(1)}ms`);
    }
    // Long-edge pages: one unit of work per frame when budget remains.
    if (performance.now() - t0 < budget) {
      const tb = performance.now();
      const g = this.longEdges.step(this.look.theme, now, budget === BUDGET_IDLE_MS);
      if (g) {
        this.tessellate(g);
        built = true;
        this.buildsThisFrame.push(`long-edges ${(performance.now() - tb).toFixed(1)}ms`);
      }
    }
    const spent = performance.now() - t0;
    this.phases.builds = (this.phases.builds ?? 0) + spent;
    this.buildLog.push([now, spent]);
    while (this.buildLog.length && this.buildLog[0][0] < now - 1000) this.buildLog.shift();
    return built;
  }

  /** Build queued code text (and any visible window whose text is cached) within `budget` ms. */
  private buildCode(now: number, budget: number): boolean {
    if (now - this.lastCodeScan > 200) {
      this.lastCodeScan = now;
      for (const w of this.client.windowsIn(this.cam.view())) {
        if (!this.code.has(w.id) && this.client.texts.has(this.client.textKey(w))) this.codeQueue.add(w.id);
      }
    }
    if (!this.codeQueue.size) return false;
    const t0 = performance.now();
    const view = grow(this.cam.view(), 400 / this.cam.zoom);
    let built = false;
    for (const id of this.codeQueue) {
      this.codeQueue.delete(id);
      const w = this.client.windows.get(id);
      const text = w && this.client.texts.get(this.client.textKey(w));
      if (!w || !text || this.code.has(id) || !intersects(w.rect, view)) continue;
      const chunk = this.chunks.get(cellKey(Math.floor(w.rect.x / CELL_WU), Math.floor(w.rect.y / CELL_WU)));
      if (!chunk) continue;
      if (isMinimized(w, this.look) || isHiddenContext(w, this.look) || this.look.hiddenLanes?.has(w.lane)) continue;
      const hits: ExpandHit[] = [];
      const c = buildCodeText(w, text, this.look.theme, this.client.diffs.get(w.id), hits, this.look.ownerSlot);
      c.alpha = 0;
      chunk.parts.code.addChild(c);
      this.code.set(id, { c, chunk, born: now, hits });
      this.fading.push({ c, born: now });
      built = true;
      if (performance.now() - t0 > budget) break;
    }
    // Keep a bounded number of built windows; drop the oldest that are off screen.
    if (this.code.size > this.codeCap) {
      for (const [id, e] of this.code) {
        if (this.code.size <= this.codeCap * 0.8) break;
        const w = this.client.windows.get(id);
        if (w && intersects(w.rect, view)) continue;
        dispose(e.c);
        this.code.delete(id);
      }
    }
    return built;
  }

  /**
   * Pixi tessellates Graphics lazily at their first render. Doing it here instead keeps that cost
   * inside the build budget rather than in whichever frame first shows the geometry.
   */
  private tessellate(c: Container) {
    if (c instanceof Graphics) this.app.renderer.graphicsContext.updateGpuContext(c.context);
    else for (const ch of c.children) this.tessellate(ch);
  }

  private applyWheel(dt: number) {
    const w = this.wheel;
    if (!w) return;
    // Ease toward the wheel target: notched wheels zoom smoothly instead of in jumps.
    const f = 1 - Math.exp(-dt / 55);
    const step = Math.abs(w.log) < 0.002 ? w.log : w.log * f;
    w.log -= step;
    this.cam.zoomAt(w.sx, w.sy, Math.exp(step));
    if (w.log === 0) this.wheel = undefined;
  }

  stats(): EngineStats {
    const ft = [...this.frameTimes].sort((a, b) => a - b);
    const q = (p: number) => ft[Math.min(ft.length - 1, Math.floor(p * ft.length))] ?? 0;
    const mean = ft.reduce((a, b) => a + b, 0) / Math.max(1, ft.length);
    const parts = this.neededParts();
    const pendingBuilds = [...this.visible].filter((c) => this.outlineCells.has(c.key) || parts.some((p) => c.dirty.has(p))).length;
    let windowsVisible = 0;
    for (const c of this.visible) windowsVisible += c.windows.length;
    return {
      fps: mean ? 1000 / mean : 0,
      frameP50: q(0.5),
      frameP95: q(0.95),
      frameMax: ft[ft.length - 1] ?? 0,
      tier: this.tier,
      zoom: this.cam.zoom,
      windowsLoaded: this.client.windows.size,
      windowsTotal: this.client.view.stats.windows,
      windowsVisible,
      chunksVisible: this.visible.size,
      pendingRequests: this.client.loader.pending,
      pendingBuilds,
      pendingSwitch: this.switching > 0,
      buildMsLastSecond: this.buildLog.reduce((a, b) => a + b[1], 0),
      palette: paletteUse(),
    };
  }

  private bindInput() {
    const c = this.app.canvas;
    c.style.touchAction = "none";
    let lastClick: { t: number; x: number; y: number } | undefined;
    let drag: { id: number; x: number; y: number; t: number; moved: number; vx: number; vy: number } | undefined;
    // Moving the selected window by its title bar (L2+) or the selected cluster (L0/L1): a click
    // still selects; past 5 px it becomes a move, pinned where it is dropped.
    let moveDrag: { id: number; key: string; rect: Rect; gx: number; gy: number; started: boolean } | undefined;
    const startMoveDrag = (e: PointerEvent) => {
      moveDrag = undefined;
      if (e.button !== 0 || e.pointerType === "touch" || this.isRange) return;
      const sel = this.selected;
      const [wx, wy] = this.cam.toWorld(e.offsetX, e.offsetY);
      const inside = (r: Rect, h = r.h) => wx >= r.x && wx <= r.x + r.w && wy >= r.y && wy <= r.y + h;
      if (sel?.type === "window" && tierIndex(this.tier) >= 2 && !sel.w.flags.minimized && inside(sel.w.rect, HEADER_WU)) {
        moveDrag = { id: e.pointerId, key: sel.w.key, rect: sel.w.rect, gx: wx - sel.w.rect.x, gy: wy - sel.w.rect.y, started: false };
      } else if (sel?.type === "cluster" && tierIndex(this.tier) <= 1 && inside(sel.c.rect)) {
        moveDrag = { id: e.pointerId, key: `cluster:${sel.c.id}`, rect: sel.c.rect, gx: wx - sel.c.rect.x, gy: wy - sel.c.rect.y, started: false };
      }
    };
    // Dragging lines: a range handle (fixed = the other end), or a new range from the gutter.
    let lineDrag: { id: number; w: WindowHeader; fixed: number; compose: boolean } | undefined;
    const startLineDrag = (e: PointerEvent): boolean => {
      if (e.button !== 0 || e.pointerType === "touch") return false;
      const [wx, wy] = this.cam.toWorld(e.offsetX, e.offsetY);
      const r = this.rangeMark.range;
      const edge = r?.editable && this.rangeMark.root.visible ? this.rangeMark.handleAt(wx, wy) : undefined;
      if (r && edge) {
        lineDrag = { id: e.pointerId, w: r.w, fixed: edge === "start" ? r.end : r.start, compose: false };
        return true;
      }
      if (this.tier !== "L3") return false;
      const t = this.pick(e.offsetX, e.offsetY);
      if (t?.type !== "window" || t.w.flags.minimized) return false;
      if (wy <= t.w.rect.y + HEADER_WU || wx >= t.w.rect.x + GUTTER_WU) return false;
      // Press in the gutter: drag over lines to comment on a range (a click is one line).
      const line = this.lineAt(t.w, wy);
      lineDrag = { id: e.pointerId, w: t.w, fixed: line, compose: true };
      this.setCommentRange({ w: t.w, start: line, end: line, editable: false });
      return true;
    };
    // Touch: two pointers pinch-zoom about their midpoint and pan with it.
    const touches = new Map<number, { x: number; y: number }>();
    let pinch: { d: number; mx: number; my: number } | undefined;
    const pinchState = () => {
      const [a, b] = [...touches.values()];
      return { d: Math.hypot(a.x - b.x, a.y - b.y), mx: (a.x + b.x) / 2, my: (a.y + b.y) / 2 };
    };
    c.addEventListener("pointerdown", (e) => {
      if (e.pointerType === "touch") {
        touches.set(e.pointerId, { x: e.offsetX, y: e.offsetY });
        if (touches.size === 2) {
          pinch = pinchState();
          drag = undefined;
          return;
        }
      }
      c.setPointerCapture(e.pointerId);
      this.still = undefined;
      this.dwellCue.hidden = true;
      this.cam.stop();
      this.wheel = undefined;
      if (startLineDrag(e)) return;
      startMoveDrag(e);
      drag = { id: e.pointerId, x: e.offsetX, y: e.offsetY, t: e.timeStamp, moved: 0, vx: 0, vy: 0 };
    });
    c.addEventListener("pointermove", (e) => {
      if (touches.has(e.pointerId)) touches.set(e.pointerId, { x: e.offsetX, y: e.offsetY });
      if (pinch && touches.size === 2) {
        const p = pinchState();
        this.cam.panBy(p.mx - pinch.mx, p.my - pinch.my);
        if (pinch.d > 0 && p.d > 0) this.cam.zoomAt(p.mx, p.my, p.d / pinch.d);
        pinch = p;
        return;
      }
      if (lineDrag && lineDrag.id === e.pointerId) {
        const [, wy] = this.cam.toWorld(e.offsetX, e.offsetY);
        const r: LineRange = { w: lineDrag.w, start: lineDrag.fixed, end: this.lineAt(lineDrag.w, wy), editable: !lineDrag.compose };
        const cur = this.rangeMark.range;
        if (!cur || Math.min(r.start, r.end) !== cur.start || Math.max(r.start, r.end) !== cur.end) {
          this.setCommentRange(r);
          if (!lineDrag.compose) this.onCommentRange?.(this.rangeMark.range!);
        }
        return;
      }
      if (!drag) {
        this.pointer = { x: e.offsetX, y: e.offsetY, moved: true };
        const st = this.still;
        if (!st || Math.abs(st.x - e.offsetX) + Math.abs(st.y - e.offsetY) > GATHER.stillPx) {
          this.still = { x: e.offsetX, y: e.offsetY, at: performance.now() };
          this.dwellCue.hidden = true;
        }
        const r = this.rangeMark.range;
        if (r?.editable && this.rangeMark.root.visible) {
          const [wx, wy] = this.cam.toWorld(e.offsetX, e.offsetY);
          c.style.cursor = this.rangeMark.handleAt(wx, wy) ? "ns-resize" : this.hover ? "pointer" : "";
        }
        this.tooltip.move(e.offsetX, e.offsetY);
        return;
      }
      if (drag.id !== e.pointerId) return;
      if (moveDrag && moveDrag.id === e.pointerId) {
        drag.moved += Math.abs(e.offsetX - drag.x) + Math.abs(e.offsetY - drag.y);
        drag.x = e.offsetX;
        drag.y = e.offsetY;
        if (drag.moved > 5) {
          moveDrag.started = true;
          c.style.cursor = "move";
          const [wx, wy] = this.cam.toWorld(e.offsetX, e.offsetY);
          this.drawMoveGhost({ ...moveDrag.rect, x: wx - moveDrag.gx, y: wy - moveDrag.gy });
        }
        return;
      }
      if (drag.moved > 4) c.style.cursor = "grabbing";
      const dx = e.offsetX - drag.x;
      const dy = e.offsetY - drag.y;
      const dt = Math.max(1, e.timeStamp - drag.t);
      drag.moved += Math.abs(dx) + Math.abs(dy);
      drag.vx = drag.vx * 0.6 + (dx / dt) * 0.4;
      drag.vy = drag.vy * 0.6 + (dy / dt) * 0.4;
      drag.x = e.offsetX;
      drag.y = e.offsetY;
      drag.t = e.timeStamp;
      this.cam.panBy(dx, dy);
    });
    c.addEventListener("pointerleave", () => {
      this.pointer = undefined;
      this.still = undefined;
      this.dwellCue.hidden = true;
      if (!this.gather.pinned) this.releaseGather();
      this.setHover(undefined);
    });
    const end = (e: PointerEvent) => {
      touches.delete(e.pointerId);
      if (touches.size < 2) pinch = undefined;
      if (lineDrag && lineDrag.id === e.pointerId) {
        const r = this.rangeMark.range;
        if (lineDrag.compose && r) {
          this.select({ type: "window", w: lineDrag.w });
          this.setCommentRange({ ...r, editable: true });
          this.onAction?.({ type: "compose", window: lineDrag.w, line: r.start, end: r.end });
        }
        lineDrag = undefined;
        return;
      }
      if (!drag || drag.id !== e.pointerId) return;
      if (moveDrag?.started && moveDrag.id === e.pointerId) {
        const [wx, wy] = this.cam.toWorld(e.offsetX, e.offsetY);
        const m = moveDrag;
        moveDrag = undefined;
        drag = undefined;
        c.style.cursor = "";
        this.moveTo(m.key, wx - m.gx, wy - m.gy, true);
        return;
      }
      moveDrag = undefined;
      c.style.cursor = this.hover ? "pointer" : "";
      if (drag.moved < 5) {
        // PointerEvent.detail is 0 on pointerup, so count clicks here: a second click within
        // 350 ms and 6 px is a double click.
        const t = e.timeStamp;
        const double = lastClick && t - lastClick.t < 350 && Math.hypot(e.offsetX - lastClick.x, e.offsetY - lastClick.y) < 6;
        lastClick = double ? undefined : { t, x: e.offsetX, y: e.offsetY };
        this.click(e.offsetX, e.offsetY, double ? 2 : 1);
      }
      else if (e.timeStamp - drag.t < 60) this.cam.setVelocity(drag.vx, drag.vy);
      drag = undefined;
    };
    c.addEventListener("pointerup", end);
    c.addEventListener("pointercancel", end);
    c.addEventListener(
      "wheel",
      (e) => {
        e.preventDefault();
        const unit = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 800 : 1;
        if (e.shiftKey && !e.ctrlKey) {
          this.cam.panBy(-(e.deltaY || e.deltaX) * unit, 0);
          return;
        }
        // Pinch gestures arrive as ctrl+wheel with small deltas; scale them up.
        const k = e.ctrlKey ? 0.01 : 0.0018;
        const log = -e.deltaY * unit * k;
        this.cam.stop();
        this.wheel = { log: (this.wheel?.log ?? 0) + log, sx: e.offsetX, sy: e.offsetY };
      },
      { passive: false },
    );
    // Two-key chords: "]f" / "[f" move between the selected file's fragments (D14).
    let chord: { key: string; at: number } | undefined;
    window.addEventListener("keydown", (e) => {
      if ((e.target as HTMLElement).closest?.("input,textarea,[contenteditable]")) return;
      const step = 120;
      const k = e.key;
      if (chord && k === "f" && e.timeStamp - chord.at < 1200) {
        const dir = chord.key === "]" ? 1 : -1;
        chord = undefined;
        e.preventDefault();
        this.jumpFragment(dir);
        return;
      }
      chord = (k === "]" || k === "[") && this.selected?.type === "window" && this.selected.w.fragment ? { key: k, at: e.timeStamp } : undefined;
      if (chord) return;
      if (k === "ArrowLeft") this.cam.panBy(step, 0);
      else if (k === "ArrowRight") this.cam.panBy(-step, 0);
      else if (k === "ArrowUp") this.cam.panBy(0, step);
      else if (k === "ArrowDown") this.cam.panBy(0, -step);
      else if (k === "+" || k === "=") this.wheel = { log: 0.5, sx: this.cam.width / 2, sy: this.cam.height / 2 };
      else if (k === "-") this.wheel = { log: -0.5, sx: this.cam.width / 2, sy: this.cam.height / 2 };
      else if (k === "f") this.fit();
      else if (k === "h") this.home();
      else if (k === "Escape") {
        // Esc closes a gather first (and keeps that focus from gathering again until the pointer moves on).
        if (this.gather.active) {
          this.gatherDismissed = this.gather.key;
          this.releaseGather();
        } else this.select(undefined);
      } else if (k === "g" && this.look.lens === "cochange") {
        if (this.gather.active) this.releaseGather();
        else this.startGather(true);
      } else return;
      e.preventDefault();
    });
  }

  private click(sx: number, sy: number, detail: number) {
    this.tooltip.hide();
    if (this.gather.active) {
      // A card flies to where its file or cluster lives; a click elsewhere closes the tray.
      const [wx, wy] = this.cam.toWorld(sx, sy);
      const h = this.gather.hit(wx, wy);
      if (typeof h === "number") this.gatherGo(h);
      else if (h === undefined) this.releaseGather();
      return;
    }
    const hit = this.hud.miniHit(sx, sy);
    if (hit) {
      const cam = this.cam;
      cam.animateTo(hit[0] - cam.width / 2 / cam.zoom, hit[1] - cam.height / 2 / cam.zoom, cam.zoom, 350);
      return;
    }
    const t = this.pick(sx, sy);
    if (t?.type === "expand") {
      if (t.h.req.collapse) this.collapseWindow(t.h.window);
      else this.expandWindow(t.h.window, t.h.req);
      return;
    }
    if (t?.type === "conflict") {
      this.select({ type: "window", w: t.h.window });
      this.onAction?.({ type: "conflict", conflict: t.h.conflict, window: t.h.window });
      return;
    }
    if (t?.type === "thread") {
      this.select({ type: "window", w: t.h.window });
      this.onAction?.({ type: "thread", thread: t.h.thread, window: t.h.window });
      return;
    }
    if (t?.type === "window" && t.w.flags.minimized) {
      // Context pills expand in place on click (and collapse on the next).
      this.toggleExpanded(t.w);
      this.select(t);
      return;
    }
    if (t?.type === "window" && this.tier === "L3") {
      const [wx, wy] = this.cam.toWorld(sx, sy);
      if (wy > t.w.rect.y + HEADER_WU && wx < t.w.rect.x + GUTTER_WU) {
        // The gutter starts a review thread at that line.
        this.select(t);
        this.onAction?.({ type: "compose", window: t.w, line: this.lineAt(t.w, wy) });
        return;
      }
      if (detail >= 2) {
        this.select(t);
        this.onAction?.({ type: "edit", window: t.w });
        return;
      }
    }
    if (t?.type === "port") {
      const links = this.edgesOf(t);
      if (links.length) this.follow(links[0], t.p.window.id);
      return;
    }
    if (t?.type === "edge") {
      // Clicking a link follows it to the end farther from the pointer.
      const [wx, wy] = this.cam.toWorld(sx, sy);
      const nearFrom = Math.hypot(wx - t.e.ax, wy - t.e.ay) < Math.hypot(wx - t.e.bx, wy - t.e.by);
      this.follow(t.e, nearFrom ? t.e.from : t.e.to);
      return;
    }
    this.select(t);
    if (t && detail >= 2) this.focusTarget(t);
  }

  /**
   * Co-change gather, per frame: start one when the pointer has rested on a focus with partners
   * (a cue ring fills from GATHER.cueMs to GATHER.dwellMs), animate it, and release an unpinned one
   * once the pointer has left the focus and tray for GATHER.leaveMs. L3 gathers only on G.
   */
  private stepGather(now: number, moving: boolean) {
    const g = this.gather;
    if (g.active) {
      if (g.tick(now)) this.dirty = true;
      // The cards are sized for the zoom they opened at: zoomed far from it, put them back.
      // (The same focus does not gather again until the pointer moves to something else.)
      if (g.state !== "out" && Math.abs(Math.log(this.cam.zoom / g.zoom)) > 0.47) {
        this.gatherDismissed = g.key;
        this.releaseGather();
      }
      if (g.state === "held" && !g.pinned && !moving) {
        const p = this.pointer;
        const inside = !!p && g.inRegion(...this.cam.toWorld(p.x, p.y));
        if (inside) this.gatherLeft = 0;
        else if (!this.gatherLeft) this.gatherLeft = now;
        else if (now - this.gatherLeft > GATHER.leaveMs) this.releaseGather();
      }
      return;
    }
    const st = this.still;
    const focus = this.cochangeFor;
    if (focus !== this.gatherDismissed) this.gatherDismissed = undefined;
    // The rest counts from when the pointer, the camera and the thing under the pointer all stopped
    // changing: a pointer left still through a flight does not gather on landing.
    if (st && (moving || focus !== this.dwellFocus)) {
      st.at = performance.now();
      this.dwellFocus = focus;
    }
    if (moving || !st || !this.pointer || this.look.lens !== "cochange" || this.tier === "L3" || !focus || focus === this.gatherDismissed || !this.hover) {
      this.dwellCue.hidden = true;
      return;
    }
    const rest = performance.now() - st.at;
    if (rest >= GATHER.dwellMs) {
      this.dwellCue.hidden = true;
      this.still = { ...st, at: Infinity };
      this.startGather(false);
    } else if (rest >= GATHER.cueMs && this.dwellCue.hidden && this.gatherItems(focus)?.items.length) {
      this.dwellCue.style.transform = `translate(${st.x - 13}px, ${st.y - 13}px)`;
      this.dwellCue.style.setProperty("--dwell", `${GATHER.dwellMs - GATHER.cueMs}ms`);
      this.dwellCue.hidden = false;
    }
  }

  /** Gather the partners of the co-change focus (or of `key`: "f:<path>" / "c:<cluster id>"). */
  startGather(pinned: boolean, key = this.cochangeFor) {
    if (!key || this.look.lens !== "cochange") return false;
    const set = this.gatherItems(key);
    if (!set?.items.length) return false;
    const cam = this.cam;
    this.tooltip.hide();
    this.dwellCue.hidden = true;
    this.gatherLeft = 0;
    this.gather.start(performance.now(), {
      key,
      title: set.title,
      focus: set.focus,
      items: set.items,
      more: Math.max(0, set.items.length - GATHER.cap),
      theme: this.look.theme,
      px: 1 / cam.zoom,
      view: cam.view(),
      toScreen: (x, y) => cam.toScreen(x, y),
      area: { x: RAIL_PX + 12, y: COLHEAD_PX + 12, w: cam.width - RAIL_PX - 24, h: cam.height - COLHEAD_PX - 24 },
      pinned,
    });
    this.dirty = true;
    return true;
  }

  releaseGather(instant = false) {
    if (!this.gather.active) return;
    this.gather.release(performance.now(), instant);
    this.tooltip.hide();
    this.app.canvas.style.cursor = "";
    this.dirty = true;
    // The map under the pointer reacts again.
    if (this.pointer) this.pointer.moved = true;
  }

  /** What the gather shows (e2e checks and the bench). */
  get gatherStats() {
    return { state: this.gather.state, pinned: this.gather.pinned, hot: this.gather.hot, ...this.gather.stats };
  }

  gatherSlots() {
    return this.gather.slotRects();
  }

  /** A gathered card clicked: close the tray and fly to where the file or cluster lives. */
  private gatherGo(i: number) {
    const it = this.gather.item(i);
    this.releaseGather();
    if (!it) return;
    if (it.key.startsWith("f:")) {
      void this.goTo({ path: it.key.slice(2) });
      return;
    }
    const c = this.client.boot.clusters.find((x) => x.id === it.key.slice(2));
    if (c) {
      this.select({ type: "cluster", c });
      this.frameRect(grow(c.rect, 200), 600);
    }
  }

  /** The tray's contents for a focus: its partners as cards, strongest first, and where the focus is. */
  private gatherItems(key: string): { title: string; focus: Rect; items: GatherItem[] } | undefined {
    const co = this.cochange;
    if (!co) return undefined;
    const t = this.look.theme;
    const boot = this.client.boot;
    const byId = new Map<string, Cluster>(boot.clusters.map((c) => [c.id, c]));
    const hidden = this.look.hiddenLanes;
    const laneName = (id: string) => boot.laneDefs.find((l) => l.id === id)?.label ?? id;
    const l0 = this.tier === "L0";
    if (key.startsWith("c:")) {
      const f = byId.get(key.slice(2));
      if (!f) return undefined;
      const card = (r: Rect) => (l0 ? r : grow(r, 30));
      const items = clusterPartners(co, f.id, byId)
        .filter((p) => !hidden?.has(p.c.lane))
        .map((p) => ({
          key: "c:" + p.c.id,
          label: p.c.label,
          detail: `${laneName(p.c.lane)} · ${p.c.windows} windows`,
          p: p.p,
          k: p.k,
          origins: [card(p.c.rect)],
          from: card(p.c.rect),
          color: heatColor(t, clusterHeatLevel(p.c)),
          lines: [],
        }));
      return { title: f.label, focus: card(f.rect), items };
    }
    const path = key.slice(2);
    const own = this.client.windowsByPath.get(path) ?? [];
    // The fragment pointed at (or selected) is the focus; the tray goes beside it.
    const winOf = (x?: Target) =>
      x?.type === "window" ? x.w : x?.type === "port" ? x.p.window : x?.type === "thread" || x?.type === "conflict" || x?.type === "expand" ? x.h.window : undefined;
    const pointed = [winOf(this.hover), winOf(this.selected)].find((w) => w?.path === path);
    const hov = pointed ?? own[0];
    if (!hov) return undefined;
    const items: GatherItem[] = [];
    const list = [...(co.files.get(path) ?? [])].map(([q, p]) => ({ q, p, k: coStrength(p, "file") })).sort((a, b) => b.k - a.k);
    for (const { q, p, k } of list) {
      const ws = (this.client.windowsByPath.get(q) ?? []).filter((w) => !hidden?.has(w.lane));
      const c = byId.get(co.fileCluster.get(q) ?? "");
      const from = ws[0]?.rect ?? (c ? grow(c.rect, 30) : undefined);
      if (!from) continue;
      const names = [...new Set(ws.map((w) => w.symbol?.name ?? `lines ${w.lines.start}-${w.lines.end}`))];
      items.push({
        key: "f:" + q,
        label: basename(q),
        detail: q.slice(0, q.lastIndexOf("/") + 1) || "./",
        p,
        k,
        origins: ws.map((w) => w.rect),
        from,
        color: ws[0] ? heatColor(t, heatLevel(ws[0].heat.grade, ws[0].heat.rate, this.look.churn)) : c ? heatColor(t, clusterHeatLevel(c)) : t.line,
        lines: names.length > GATHER.maxLines ? [...names.slice(0, GATHER.maxLines - 1), `+${names.length - GATHER.maxLines + 1} more`] : names,
      });
    }
    // Files of one name (every package's settings.yaml) are told apart by their folder.
    const seen = new Map<string, number>();
    for (const it of items) seen.set(it.label, (seen.get(it.label) ?? 0) + 1);
    for (const it of items) if ((seen.get(it.label) ?? 0) > 1) it.label = it.key.slice(2).split("/").slice(-2).join("/");
    return { title: basename(path), focus: hov.rect, items };
  }

  /** What is under screen point (sx, sy): windows, then links, highways, hubs, clusters. */
  pick(sx: number, sy: number): Target | undefined {
    if (sx < RAIL_PX || sy < COLHEAD_PX || this.hud.miniHit(sx, sy)) return undefined;
    const cam = this.cam;
    const [x, y] = cam.toWorld(sx, sy);
    const tol = 6 / cam.zoom;
    const tol2 = tol * tol;
    const tier = tierIndex(this.tier);
    const hidden = this.look.hidden;
    if (tier >= 3) {
      for (const e of this.code.values()) {
        for (const h of e.hits) if (x >= h.x && x <= h.x + h.w && y >= h.y && y <= h.y + h.h) return { type: "expand", h };
      }
    }
    if (tier >= 2) {
      for (const h of this.threadMarks.conflictHits) {
        if (x >= h.x && x <= h.x + h.w && y >= h.y && y <= h.y + h.h) return { type: "conflict", h };
      }
      for (const h of this.threadMarks.hits) {
        if (x >= h.x && x <= h.x + h.w && y >= h.y && y <= h.y + h.h) return { type: "thread", h };
      }
      // Port pills straddle window borders, so they win over the window under them.
      for (const c of this.visible) {
        for (const p of c.portHits) {
          if (x >= p.x && x <= p.x + p.w && y >= p.y && y <= p.y + p.h) return { type: "port", p };
        }
      }
    }
    if (tier >= 1) {
      const w = this.client.windowsIn({ x, y, w: 1, h: 1 }).find((w) => !isHiddenContext(w, this.look) && !this.look.hiddenLanes?.has(w.lane));
      if (w) return { type: "window", w };
      let best: Edge | undefined;
      let bestD = tol2;
      const consider = (e: Edge) => {
        if (hidden.has(GROUP_OF[e.kind]) || (tier < 2 && e.confidence < LOW_CONFIDENCE)) return;
        const d = distSq(edgePoints(e), x, y);
        if (d < bestD) {
          bestD = d;
          best = e;
        }
      };
      for (const c of this.visibleEdges) {
        const b = c.edgeBounds;
        if (x < b.x - tol || x > b.x + b.w + tol || y < b.y - tol || y > b.y + b.h + tol) continue;
        for (const e of c.edges) consider(e);
      }
      for (const e of this.client.longEdges) {
        if (x < Math.min(e.ax, e.bx) - 2000 || x > Math.max(e.ax, e.bx) + 2000) continue;
        consider(e);
      }
      if (best) return { type: "edge", e: best };
    }
    // At L0 highways run beneath the cluster fills: a point on a cluster is the cluster.
    const cluster = this.client.boot.clusters.find((c) => x >= c.rect.x && x <= c.rect.x + c.rect.w && y >= c.rect.y && y <= c.rect.y + c.rect.h);
    if (tier === 0 && cluster) return { type: "cluster", c: cluster };
    let hw: Target | undefined;
    let hwD = tol2 * 1.5;
    for (const h of this.bundles.highways) {
      const b = h.box;
      if (!h.count || x < b.x - tol || x > b.x + b.w + tol || y < b.y - tol || y > b.y + b.h + tol) continue;
      const d = distSq(h.pts, x, y);
      if (d < hwD) {
        hwD = d;
        hw = { type: "highway", h };
      }
    }
    if (hw) return hw;
    for (const h of this.client.boot.hubs) {
      const r = h.rect;
      if (r && x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h) return { type: "hub", h };
    }
    return cluster && { type: "cluster", c: cluster };
  }

  private updateHover() {
    const p = this.pointer;
    if (this.gather.active) {
      // While gathered the map under the scrim does not react; the cards do.
      const [wx, wy] = p ? this.cam.toWorld(p.x, p.y) : [NaN, NaN];
      const h = p ? this.gather.hit(wx, wy) : undefined;
      const i = typeof h === "number" ? h : -1;
      this.gather.setHot(i);
      const it = this.gather.item(i);
      this.app.canvas.style.cursor = it ? "pointer" : "";
      if (it && p) this.tooltip.show("g:" + it.key, () => gatherTip(it), p.x, p.y);
      else this.tooltip.hide();
      this.dirty = true;
      return;
    }
    this.setHover(p ? this.pick(p.x, p.y) : undefined);
  }

  private setHover(t?: Target) {
    const p = this.pointer;
    if (targetKey(t) !== targetKey(this.hover)) {
      this.hover = t;
      this.overlayDirty = true;
      this.app.canvas.style.cursor = t ? "pointer" : "";
      this.noteCochangeFocus();
    }
    if (t && p) this.tooltip.show(targetKey(t), () => describe(t, this.client, this.edgesOf(t)), p.x, p.y);
    else this.tooltip.hide();
  }
}

function basename(path: string) {
  return path.slice(path.lastIndexOf("/") + 1);
}

const escHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;");

/** Tooltip of a gathered card: what it is and how surely it changes with the focus. */
function gatherTip(it: GatherItem) {
  const union = it.p.jaccard > 0 ? Math.round(it.p.support / it.p.jaccard) : it.p.support;
  return `<div class="t">${escHtml(it.label)}</div><div class="mono dim">${escHtml(it.detail)}</div><div>changed together in ${it.p.support} of ${union} commits <span class="dim">(jaccard ${pct(it.p.jaccard)})</span></div><div class="dim">${it.p.linked ? "also joined by static links" : "no static link between them"}</div><div class="hint">click to go there · Esc closes</div>`;
}
