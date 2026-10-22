import { Container } from "pixi.js";
import type { Lens, Link, Rect, WindowDiff, WindowHeader } from "../contracts/model";
import { COL_WU, HEADER_WU, LINE_WU, OUTLINE_RECORD_BYTES, TokenClass } from "../contracts/wire";
import type { Edge, Outline } from "../data/client";
import { GROUP_OF, LOW_CONFIDENCE, dashedPolyline, edgeColor, edgePoints, quadEdgeSteps, type LinkGroup } from "./edges";
import type { RangeOptions } from "../contracts/model";
import { RectBatch, TL, TR, dispose } from "./rects";
import { LabelBatch, fit, monoFont, sansFont, textWidth } from "./labels";
import { CHIP_HI, CHIP_REST, OWNER, heatColor, heatLevel, mix, step, type Theme } from "./theme";

export type Part = "frames" | "edges" | "sil" | "ports" | "titles" | "heads";
/** Containers per chunk: the built parts plus `code`, which holds per-window L3 text. */
export type Layer = Part | "code";
export const PARTS: Part[] = ["frames", "edges", "titles", "heads", "sil", "ports"];

export const GUTTER_WU = 54;

/** Inputs that change how a chunk looks without changing layout (lens, selection, stroke scale). */
export interface Look {
  theme: Theme;
  /** Zoom the strokes were sized for; rebuilt when the tier changes. */
  strokeZoom: number;
  tierIndex: number;
  selectedPath?: string;
  /** Link groups switched off in the legend. */
  hidden: ReadonlySet<LinkGroup>;
  /** Emphasis lens: fades windows outside it, never moves anything. */
  lens: Lens;
  /** The view's churn summary (rate bands, top rate): windows' place on the heat ramp. */
  churn?: { bands: readonly number[]; topRate?: number };
  /** Windows by id: links whose two ends a lens fades step back with them. */
  windowOf?: (id: string) => WindowHeader | undefined;
  /** Hotspots brush: clusters kept at full strength (others fade). */
  brush?: ReadonlySet<string>;
  /** Range views: diff lines per hunk window, and context windows the user expanded. */
  diffOf?: (id: string) => WindowDiff | undefined;
  /** Lanes switched off in the legend: their windows are not drawn (nothing moves). */
  hiddenLanes?: ReadonlySet<string>;
  /** Links from outline batches (dynamic targets draw as open stubs). */
  linkOf?: (id: string) => Link | undefined;
  expanded?: ReadonlySet<string>;
  /** Range views: how context windows start (RangeOptions.contextDisplay, applied client-side too). */
  contextDisplay?: RangeOptions["contextDisplay"];
  /** Range views: stripe slot of the commit that last touched a line (in-range blame). */
  ownerSlot?: (owner: string) => number;
}

/** A range-view context window drawn as a header-only pill until expanded (no relayout). */
export function isMinimized(w: WindowHeader, look: Pick<Look, "expanded" | "contextDisplay">) {
  return !!w.flags.minimized && look.contextDisplay !== "expanded" && !look.expanded?.has(w.id);
}

/** Context windows of a range view opened with contextDisplay "hidden" (the server may still send them). */
export function isHiddenContext(w: WindowHeader, look: Pick<Look, "contextDisplay">) {
  return w.kind === "context" && look.contextDisplay === "hidden";
}

/** Owner stripe colour of a range commit (in-range blame): a small categorical set, never heat hues. */
export function ownerColor(slot: number) {
  return OWNER[slot % OWNER.length];
}

export const DIFF_ADD = 0x45c08a;
export const DIFF_DEL = 0xf0625a;

/** Added-line tints and removed-line markers of a hunk window (shared with L3 code text). */
export function drawDiff(rb: RectBatch, w: WindowHeader, diff: WindowDiff | undefined, wuPerPx: number, ownerSlot?: (owner: string) => number, stats?: ChunkStats) {
  if (!diff) return;
  // Owner stripes (D16): which commit of the range last touched each line, at the gutter's edge.
  if (ownerSlot) {
    const sw = Math.max(5, 3 * wuPerPx);
    const x = w.rect.x + GUTTER_WU - sw - 4;
    for (const d of diff.lines) {
      if (!d.owner || d.newLine === undefined || d.newLine < w.lines.start || d.newLine > w.lines.end) continue;
      rb.add(x, w.rect.y + HEADER_WU + (d.newLine - w.lines.start) * LINE_WU, sw, LINE_WU, ownerColor(ownerSlot(d.owner)), 0.95);
      if (stats) stats.ownerStripes++;
    }
  }
  let lastNew = w.lines.start - 1;
  let dels = 0;
  const flushDels = () => {
    if (!dels) return;
    const y = w.rect.y + HEADER_WU + (lastNew + 1 - w.lines.start) * LINE_WU;
    rb.add(w.rect.x + 2, y - Math.max(2, 1.5 * wuPerPx), w.rect.w - 4, Math.max(4, 3 * wuPerPx), DIFF_DEL, 0.85);
    dels = 0;
  };
  for (const d of diff.lines) {
    if (d.kind === "del") {
      dels++;
      continue;
    }
    flushDels();
    if (d.newLine === undefined) continue;
    lastNew = d.newLine;
    if (d.kind === "add" && d.newLine >= w.lines.start && d.newLine <= w.lines.end) {
      const y = w.rect.y + HEADER_WU + (d.newLine - w.lines.start) * LINE_WU;
      rb.add(w.rect.x + 2, y, w.rect.w - 4, LINE_WU, DIFF_ADD, 0.2);
      rb.add(w.rect.x + 2, y, Math.max(4, 3 * wuPerPx), LINE_WU, DIFF_ADD, 0.9);
    }
  }
  flushDels();
}

/** Files in some co-change pair (set by the engine when the co-change lens is on). */
export const cochangePaths = new Set<string>();
/** Per paired file, the strength (cochange.ts coStrength) of its strongest partner: its tint under the lens. */
export const cochangeStrength = new Map<string, number>();

/** Whether a window is outside the active lens (drawn faded). */
export function lensFades(w: WindowHeader, lens: Lens) {
  switch (lens) {
    case "heat":
      return w.heat.grade <= 1;
    case "connectedness":
      // Only Isolated windows (no dependents) fade; Low ones still have dependents.
      return w.conn.grade === 0;
    case "hotspots":
      return !w.reviewFirst;
    case "cochange":
      return !cochangePaths.has(w.path);
    default:
      return false;
  }
}

/** What the last builds of a chunk drew, for e2e checks of lenses, ports and range stripes. */
export interface ChunkStats {
  /** Heat lens: hot windows with a thermal glow (pre-blurred quads under the card). */
  glows: number;
  /** Windows drawn faded by the lens or the Hotspots brush. */
  faded: number;
  /** Windows drawn raised past their normal elevation (connectedness lens). */
  raised: number;
  /** Largest drop-shadow offset drawn, in screen px at the tier's zoom. */
  maxElevPx: number;
  /** Widest link drawn (screen px), and links stepped back because the lens fades an end. */
  maxEdgePx: number;
  fadedLinks: number;
  /** "?" markers on low-confidence links. */
  lowMarks: number;
  /** Range views: owner-stripe quads (in-range blame). */
  ownerStripes: number;
  /** Port pills drawn, moved off their line to clear a neighbour, and merged into "×N". */
  pills: number;
  stacked: number;
  merged: number;
  /** File chips drawn (split files' fragments): window id → chip colour (e2e checks the palette). */
  chips: Map<string, number>;
}

const zeroStats = (): ChunkStats => ({ glows: 0, faded: 0, raised: 0, maxElevPx: 0, maxEdgePx: 0, fadedLinks: 0, lowMarks: 0, ownerStripes: 0, pills: 0, stacked: 0, merged: 0, chips: new Map() });

/**
 * One spatial cell of the world. Everything in it is static geometry under one render group, so
 * camera moves cost a transform, never a rebuild. Parts are rebuilt off the hot path, in budgeted
 * slices, and the old geometry stays on screen until its replacement is ready.
 */
export class Chunk {
  readonly root = new Container({ isRenderGroup: true });
  /** Short edges live in a shared layer beneath every chunk's windows; the engine adds this to it. */
  readonly edgeRoot = new Container({ isRenderGroup: true });
  readonly parts: Record<Layer, Container>;
  /** Builds in progress, resumed across frames (a part stays dirty until its build finishes). */
  private jobs = new Map<Part, Generator<void, void, void>>();
  /** Parts to rebuild. Marking a part dirty again while it is being built restarts that build. */
  readonly dirty: Set<Part> = new DirtySet((p) => this.abort(p));
  /** Bounds of the windows, and separately of the short edges, for culling the two roots. */
  bounds: Rect;
  edgeBounds: Rect;
  windows: WindowHeader[] = [];
  edges: Edge[] = [];
  /** Port pills as built (world rects), for hit testing. */
  portHits: PortHit[] = [];
  /** Last frame time this chunk was in view (LRU for releasing geometry). */
  seen = 0;
  /** Whether any part holds built geometry. */
  built = false;
  readonly stats: ChunkStats = zeroStats();

  constructor(
    readonly key: string,
    cell: Rect,
  ) {
    this.bounds = { ...cell, w: 0, h: 0 };
    this.edgeBounds = { ...cell, w: 0, h: 0 };
    this.parts = {
      frames: new Container(),
      edges: new Container(),
      sil: new Container(),
      ports: new Container(),
      titles: new Container(),
      heads: new Container(),
      code: new Container(),
    };
    // Draw order inside a chunk: frames, silhouettes, code, labels; edges go to a shared layer beneath.
    this.root.addChild(this.parts.frames, this.parts.sil, this.parts.code, this.parts.titles, this.parts.heads, this.parts.ports);
    this.edgeRoot.addChild(this.parts.edges);
    for (const p of PARTS) this.dirty.add(p);
  }

  setWindows(ws: WindowHeader[]) {
    this.windows = ws;
    let b: Rect | undefined;
    for (const w of ws) b = union(b, w.rect);
    if (b) this.bounds = b;
    for (const p of ["frames", "titles", "heads", "sil", "ports"] as Part[]) this.dirty.add(p);
  }

  setEdges(es: Edge[]) {
    // Lists only grow; extend the bounds by the edges added since last time.
    let b: Rect | undefined = this.edges.length ? this.edgeBounds : undefined;
    for (let i = this.edges.length && es.length >= this.edges.length ? this.edges.length : 0; i < es.length; i++) b = union(b, edgeBox(es[i]));
    this.edges = es.slice();
    if (b) this.edgeBounds = b;
    this.dirty.add("edges");
  }

  /** Build a part whole. */
  build(part: Part, look: Look, outlines: (id: string) => Outline | undefined) {
    this.step(part, look, outlines, Infinity);
  }

  /**
   * Build a part, or resume its build, until it is done or `deadline` (performance.now()) passes.
   * The work comes in units of one window (or a few dozen links), and a unit is not started when
   * the one before it would not fit before the deadline, so a frame keeps within its budget. The
   * old geometry stays on screen until the last unit swaps the new one in. Returns whether the
   * part is done.
   */
  step(part: Part, look: Look, outlines: (id: string) => Outline | undefined, deadline: number): boolean {
    let job = this.jobs.get(part);
    if (!job) {
      job = this.job(part, look, outlines);
      this.jobs.set(part, job);
      this.built = true;
    }
    let t = performance.now();
    for (;;) {
      if (job.next().done) {
        this.jobs.delete(part);
        this.dirty.delete(part);
        return true;
      }
      const now = performance.now();
      if (now + (now - t) >= deadline) return false;
      t = now;
    }
  }

  /** Whether a part's build has started and not finished. */
  building(part: Part) {
    return this.jobs.has(part);
  }

  private *job(part: Part, look: Look, outlines: (id: string) => Outline | undefined): Generator<void, void, void> {
    const shown =
      look.hiddenLanes?.size || look.contextDisplay === "hidden"
        ? this.windows.filter((w) => !look.hiddenLanes?.has(w.lane) && !isHiddenContext(w, look))
        : this.windows;
    switch (part) {
      case "frames":
        return yield* buildFrames(this.parts.frames, shown, look, this.stats);
      case "edges":
        return yield* buildChunkEdges(this.parts.edges, this.edges, look, this.stats);
      case "sil":
        return yield* buildSilhouettes(this.parts.sil, shown, look, outlines, this.stats);
      case "ports": {
        // Hit rects swap in with the pills they describe.
        const hits: PortHit[] = [];
        yield* buildPorts(this.parts.ports, shown, look, outlines, hits, this.stats);
        this.portHits = hits;
        return;
      }
      case "titles":
        return yield* buildTitles(this.parts.titles, shown, look);
      case "heads":
        return yield* buildHeads(this.parts.heads, shown, look);
    }
  }

  /** Abandon a part's build in progress (its scratch buffers go back to their pools). */
  private abort(part: Part) {
    const job = this.jobs.get(part);
    if (!job) return;
    this.jobs.delete(part);
    job.return();
  }

  /**
   * Free the built geometry (meshes and their typed arrays) of a chunk long out of view; its data
   * stays, and every part rebuilds on demand when the camera comes back.
   */
  release() {
    for (const p of PARTS) this.abort(p);
    for (const p of Object.values(this.parts)) for (const ch of p.removeChildren()) dispose(ch);
    this.portHits = [];
    for (const p of PARTS) this.dirty.add(p);
    this.built = false;
  }

  destroy() {
    for (const p of PARTS) this.abort(p);
    dispose(this.root);
    dispose(this.edgeRoot);
  }
}

/** A dirty set that restarts a part's build in progress when the part is marked dirty again. */
class DirtySet extends Set<Part> {
  constructor(private readonly restart: (p: Part) => void) {
    super();
  }

  override add(p: Part) {
    if (this.restart && this.has(p)) this.restart(p);
    return super.add(p);
  }
}

function union(a: Rect | undefined, b: Rect): Rect {
  if (!a) return { ...b };
  const x = Math.min(a.x, b.x);
  const y = Math.min(a.y, b.y);
  return { x, y, w: Math.max(a.x + a.w, b.x + b.w) - x, h: Math.max(a.y + a.h, b.y + b.h) - y };
}

export function edgeBox(e: Edge): Rect {
  const p = edgePoints(e);
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (let i = 0; i < p.length; i += 2) {
    if (p[i] < x0) x0 = p[i];
    if (p[i] > x1) x1 = p[i];
    if (p[i + 1] < y0) y0 = p[i + 1];
    if (p[i + 1] > y1) y1 = p[i + 1];
  }
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

const ELEV_PX = [0, 1, 2, 3.5, 5];
/** Connectedness lens: elevation multiplier per grade (central windows rise well above the rest). */
const ELEV_LENS = [1, 1, 1.7, 2.6, 3.4];
const FRAME_PX = [1, 1, 1.25, 1.75, 2.5];
/** Card corner radius on screen at the tier's stroke zoom. */
export const CARD_RADIUS_PX = 6;

/** Whether the lens or the Hotspots brush leaves `w` out (drawn faded). */
function outOfFocus(w: WindowHeader, look: Look) {
  return lensFades(w, look.lens) || (!!look.brush && !look.brush.has(w.cluster));
}

/**
 * The colour a window's heat cap and tint take: its heat on the continuous ramp, except under the
 * lenses whose subject is not heat. Depends: depended on (accent) or depending on others
 * (consumer), by fan-in against fan-out, stronger the more lopsided. Co-change: the lens colour,
 * stronger the more surely the file changes with another.
 */
export function lensColor(w: WindowHeader, look: Look) {
  const t = look.theme;
  if (look.lens === "connectedness") {
    const dir = Math.log2((w.conn.fanIn + 1) / (w.conn.fanOut + 1));
    const k = step(Math.abs(dir) / 2.5);
    return mix(mix(t.body, t.line, 0.45), dir >= 0 ? t.accent : t.consumer, 0.3 + 0.7 * k);
  }
  if (look.lens === "cochange") {
    const k = cochangeStrength.get(w.path);
    if (k !== undefined) return mix(mix(t.body, t.line, 0.45), t.couple, 0.2 + 0.5 * step(k));
  }
  return heatColor(t, heatLevel(w.heat.grade, w.heat.rate, look.churn));
}

function* buildFrames(c: Container, ws: WindowHeader[], look: Look, stats: ChunkStats): Generator<void, void, void> {
  const { theme: t, strokeZoom: z } = look;
  const px = (v: number) => v / z;
  const dark = t.name === "dark";
  const mapTint = look.tierIndex <= 1 ? (dark ? 0.5 : 0.34) : 0.06;
  const rb = new RectBatch(ws.length * 40);
  try {
    const radius = px(CARD_RADIUS_PX);
    const heatLens = look.lens === "heat";
    const connLens = look.lens === "connectedness";
    stats.glows = stats.faded = stats.raised = 0;
    stats.maxElevPx = 0;
    stats.chips.clear();
    // Heat lens: a thermal glow under warm and hot windows, drawn first so it sits beneath every
    // card, growing with the window's place on the heat ramp. Like the shadows, pre-blurred atlas
    // cells stretched over eight quads: no filter, same mesh. Glows of neighbouring hot windows
    // merge into one warm halo over a hot cluster.
    if (heatLens) {
      for (const w of ws) {
        yield;
        const lv = heatLevel(w.heat.grade, w.heat.rate, look.churn);
        if (lv < 0.6 || isMinimized(w, look) || outOfFocus(w, look)) continue;
        const k = step((lv - 0.6) / 0.4, 3);
        const r = w.rect;
        const spread = px(8 + 20 * k) + Math.min(r.w, r.h) * 0.05 * k;
        rb.soft(r.x, r.y, r.w, r.h, spread, heatColor(t, lv), (dark ? 0.3 : 0.24) + (dark ? 0.3 : 0.24) * k);
        stats.glows++;
      }
    }
    // Soft elevation shadows first, so neighbours never cover each other's frames. A pre-blurred
    // atlas cell stretched over eight quads per card: a drop shadow without a filter.
    const shadowAlpha = dark ? 0.5 : 0.24;
    for (const w of ws) {
      yield;
      if (isMinimized(w, look)) continue;
      const k = w.conn.grade;
      const lift = connLens ? ELEV_LENS[k] : 1;
      const e = ELEV_PX[k] * lift;
      const s = px(1.5 + 2 * e);
      const dy = px(0.3 + 0.5 * e);
      const r = w.rect;
      const ins = Math.min(radius * 0.6, r.w / 4, r.h / 4);
      const fade = outOfFocus(w, look);
      const a = fade ? 0.4 : connLens ? Math.min(1, 0.7 + 0.1 * k) : 0.6 + 0.1 * k;
      rb.soft(r.x + ins, r.y + ins + dy, r.w - 2 * ins, r.h - 2 * ins, s + ins, t.shadow, shadowAlpha * a);
      if (lift > 1 && !fade) {
        stats.raised++;
        stats.maxElevPx = Math.max(stats.maxElevPx, 0.3 + 0.5 * e);
      } else if (!fade) stats.maxElevPx = Math.max(stats.maxElevPx, 0.3 + 0.5 * e);
    }
    for (const w of ws) {
      yield;
      const r = w.rect;
      if (isMinimized(w, look)) {
        // Context pill: just the header, dimmed; the window's full slot stays reserved.
        const bw = px(1);
        rb.round(r.x, r.y, r.w, HEADER_WU, radius, mix(t.body, t.line, 0.35));
        rb.round(r.x + bw, r.y + bw, r.w - 2 * bw, HEADER_WU - 2 * bw, radius - bw, mix(t.header, t.bg, 0.35));
        continue;
      }
      // A lens fades what it leaves out toward the background; it never moves anything.
      const fade = outOfFocus(w, look) ? 0.76 : 0;
      if (fade) stats.faded++;
      const f = (c: number) => (fade ? mix(c, t.bg, fade) : c);
      const hc = lensColor(w, look);
      const heat = f(hc);
      const hi = !!w.fragment && look.selectedPath === w.path;
      const chip = w.fragment ? (hi ? CHIP_HI : CHIP_REST)[w.fragment.chip] : 0;
      // Frame weight carries connectedness; a selected file's fragments wear their chip colour.
      const k = w.conn.grade;
      // The connectedness lens also weighs the frames of central windows.
      const bw = hi ? px(2.5) : px(FRAME_PX[k] * (connLens && k >= 3 ? 1.5 : 1));
      const border = hi ? chip : f(mix(t.body, t.line, k >= 3 ? 0.8 : 0.32));
      const strip = Math.min(Math.max(5, px(3)), r.h / 3);
      const ri = Math.max(0, radius - bw);
      // Border, heat cap (follows the rounded top), body, header: four rounded fills, no strokes.
      rb.round(r.x, r.y, r.w, r.h, radius, border);
      rb.round(r.x, r.y, r.w, Math.min(r.h, radius + strip), radius, heat, 1, TL | TR);
      rb.round(r.x + bw, r.y + strip, r.w - 2 * bw, r.h - strip - bw, ri, f(mix(t.body, hc, mapTint)));
      const hh = HEADER_WU - strip;
      if (hh > 0 && r.h > HEADER_WU + bw) {
        rb.round(r.x + bw, r.y + strip, r.w - 2 * bw, hh, ri, f(mix(t.header, hc, mapTint * 0.6)), 1, TL | TR);
        rb.add(r.x + bw, r.y + HEADER_WU - px(1), r.w - 2 * bw, px(1), f(mix(t.header, t.line, 0.22)));
      }
      // Top 5% by risk ("review first", §4.9): a double accent outline.
      if (w.reviewFirst) {
        rb.ring(r.x - px(3.5), r.y - px(3.5), r.w + px(7), r.h + px(7), px(1.2), t.accent, 0.75);
        rb.ring(r.x - px(6.5), r.y - px(6.5), r.w + px(13), r.h + px(13), px(1), t.accent, 0.45);
      }
      // L3: the 5×5 district glyph in the header marks the window's cell (heat column, connectedness
      // row): a dim square, a faint row and column through the cell, and the cell lit. (At L2 the
      // header is a few pixels tall: the engine draws the glyph at a fixed screen size instead.)
      if (look.tierIndex >= 3 && r.h > HEADER_WU) {
        const g = DISTRICT_WU;
        const gx = r.x + 10;
        const gy = r.y + (HEADER_WU - g) / 2 + 1;
        const c = g / 5;
        const col = 4 - w.heat.grade;
        const row = 4 - w.conn.grade;
        rb.add(gx, gy, g, g, f(mix(t.header, t.line, 0.28)));
        rb.add(gx, gy + row * c, g, c, f(mix(t.header, t.line, 0.5)));
        rb.add(gx + col * c, gy, c, g, f(mix(t.header, t.line, 0.5)));
        rb.add(gx + col * c, gy + row * c, c, c, heat);
      }
      if (w.fragment) {
        const cw = Math.max(70, px(8));
        const ch = HEADER_WU - 16;
        rb.round(r.x + r.w - cw - 10, r.y + 9, cw, ch, ch / 2, chip);
        stats.chips.set(w.id, chip);
      }
      let bx = r.x + r.w - (w.fragment ? 100 : 20);
      const d = Math.max(16, px(5));
      if (w.counts.conflicts) {
        rb.round(bx - d / 2, r.y + HEADER_WU / 2 - d / 2, d, d, d / 2, t.conflict);
        bx -= d * 1.6;
      }
      if (w.counts.openThreads) rb.round(bx - d / 2, r.y + HEADER_WU / 2 - d / 2, d, d, d / 2, t.thread);
      // L1: live-edited and overlapping windows get a corner tab (the words come at L2).
      if (look.tierIndex <= 1 && (w.flags.unsnapshotted || w.flags.alsoChangedIn?.length)) {
        const tab = Math.min(r.w / 3, Math.max(60, px(10)));
        rb.round(r.x + r.w - tab, r.y + r.h - Math.min(r.h / 4, px(5)), tab, Math.min(r.h / 4, px(5)), 0, w.flags.alsoChangedIn?.length ? t.warn : t.add);
      }
    }
    yield;
    rb.build(c);
  } finally {
    rb.discard();
  }
}

/** Side of the 5×5 district glyph in a window header. */
export const DISTRICT_WU = 20;

const SIL_ALPHA = 0.55;
/** Silhouette saturation per heat grade (§4.9: heat is hue on the strip and saturation here). */
const SIL_SAT = [0.12, 0.35, 0.6, 0.82, 1];
const silCache = new Map<string, number[]>();

/** Token colours of a silhouette at heat `grade`: cold code greys out, hot code keeps its colour. */
function silPalette(t: Theme, grade: number) {
  const k = t.name + grade;
  let p = silCache.get(k);
  if (!p) {
    p = [];
    for (let cls = 0; cls < 16; cls++) {
      const c = t.token[cls] ?? t.dim;
      const r = (c >> 16) & 255, g = (c >> 8) & 255, b = c & 255;
      const y = Math.round(0.3 * r + 0.59 * g + 0.11 * b);
      p.push(mix((y << 16) | (y << 8) | y, c, SIL_SAT[grade]));
    }
    silCache.set(k, p);
  }
  return p;
}

function* buildSilhouettes(c: Container, ws: WindowHeader[], look: Look, outlines: (id: string) => Outline | undefined, stats: ChunkStats): Generator<void, void, void> {
  const t = look.theme;
  const rb = new RectBatch(ws.length * 60);
  try {
    stats.ownerStripes = 0;
    for (const w of ws) {
      yield;
      const o = outlines(w.id);
      if (!o || isMinimized(w, look)) continue;
      drawDiff(rb, w, look.diffOf?.(w.id), 1 / look.strokeZoom, look.ownerSlot, stats);
      const pal = silPalette(t, w.heat.grade);
      const s = o.silhouette;
      const x0 = w.rect.x + GUTTER_WU;
      const maxCols = (w.rect.w - GUTTER_WU - 12) / COL_WU;
      for (let i = 0, line = 0; i < s.length; i += OUTLINE_RECORD_BYTES, line++) {
        // Long files: a unit of work is at most a few hundred lines.
        if ((line & 511) === 511) yield;
        const len = s[i + 1];
        if (!len) continue;
        const cls = s[i + 2] & 15;
        if (cls === TokenClass.Blank) continue;
        const indent = Math.min(s[i], maxCols - 1);
        const cols = Math.min(len, maxCols - indent);
        rb.add(x0 + indent * COL_WU, w.rect.y + HEADER_WU + line * LINE_WU + 5, cols * COL_WU, LINE_WU - 9, pal[cls], SIL_ALPHA);
      }
    }
    yield;
    rb.build(c);
  } finally {
    rb.discard();
  }
}

/** "2 of 3 commits": how many commits of the range touch this window (D16 badge). */
export function inRangeText(w: WindowHeader) {
  const r = w.heat.inRange;
  return r ? `${r.commits} of ${r.of} commit${r.of === 1 ? "" : "s"}` : "";
}

/** "↧ 85 ↥ 12": dependents within two hops and fan-out (§4.9 connectedness badges). */
export function depsText(w: WindowHeader) {
  const c = w.conn;
  return c.dependents2 || c.fanOut ? `↧ ${c.dependents2}  ↥ ${c.fanOut}` : "";
}

function* buildTitles(c: Container, ws: WindowHeader[], look: Look): Generator<void, void, void> {
  const font = sansFont();
  const batch = new LabelBatch(font);
  try {
    for (const w of ws) {
      yield;
      const name = w.symbol?.name ?? w.path.split("/").pop()!;
      const size = Math.max(60, Math.min(150, (w.rect.w - 40) / (0.58 * Math.max(name.length, 6))));
      const text = fit(font, name, size, w.rect.w - 40);
      if (text && w.rect.h > size + HEADER_WU + 20) {
        batch.add(text, w.rect.x + 20, w.rect.y + HEADER_WU + 14, size, look.theme.ink);
        // Range views: the in-range badge under the title, readable at map zoom.
        const ir = inRangeText(w);
        if (ir && w.rect.h > size * 1.9 + HEADER_WU + 40) batch.add(fit(font, ir, size * 0.6, w.rect.w - 40), w.rect.x + 20, w.rect.y + HEADER_WU + 24 + size, size * 0.6, look.theme.accent);
      }
    }
    yield;
    batch.build(c);
  } finally {
    batch.discard();
  }
}

/**
 * Header labels (L2+): the symbol and file on the left (after the district glyph), and right-aligned
 * badges that drop out from the left when the header is too narrow: diff stat, "2 of 3 commits",
 * "also in gym", "live", "pinned", "↧ 85 ↥ 12".
 */
function* buildHeads(c: Container, ws: WindowHeader[], look: Look): Generator<void, void, void> {
  const font = monoFont();
  const batch = new LabelBatch(font);
  try {
    const t = look.theme;
    const px = 1 / look.strokeZoom;
    const d = Math.max(16, 5 * px);
    for (const w of ws) {
      yield;
      const file = w.path.split("/").pop()!;
      const left = w.rect.x + 14 + DISTRICT_WU;
      // Right edge of the text: left of the file chip and the thread/conflict dots.
      let right = w.rect.x + w.rect.w - (w.fragment ? 100 : 20) - (w.counts.conflicts ? d * 1.6 : 0) - (w.counts.openThreads ? d * 1.1 : 0) - 10;
      const minName = Math.min(160, (w.rect.w - 60) / 2);
      const segs: Array<[string, number, number]> = [];
      const stat = w.diffStat;
      // Laid out right to left: "−3" ends the header, "+12" precedes it.
      if (stat) segs.push([`−${stat.removed}`, DIFF_DEL, 19], [`+${stat.added}`, DIFF_ADD, 19]);
      const ir = inRangeText(w);
      if (ir) segs.push([ir, t.accent, 16]);
      if (w.flags.alsoChangedIn?.length) segs.push([`also in ${w.flags.alsoChangedIn.join(", ")}`, t.warn, 15]);
      if (w.flags.unsnapshotted) segs.push(["edited, live", t.add, 15]);
      if (w.flags.minimized) segs.push(["unchanged", t.dim, 15]);
      if (w.flags.pinned) segs.push(["pinned", t.accent, 15]);
      const deps = depsText(w);
      if (deps) segs.push([deps, t.dim, 15]);
      for (const [text, color, size] of segs) {
        const tw = textWidth(font, text, size);
        if (right - tw < left + minName) break;
        batch.add(text, right - tw, w.rect.y + 9 + (19 - size) * 0.6, size, color);
        right -= tw + 12;
      }
      const maxW = right - left;
      const sym = w.symbol ? fit(font, w.symbol.qualname, 19, maxW) : "";
      if (sym) batch.add(sym, left, w.rect.y + 9, 19, t.ink);
      const used = sym ? textWidth(font, sym + "  ", 19) : 0;
      const rest = fit(font, file, 19, maxW - used);
      if (rest) batch.add(rest, left + used, w.rect.y + 9, 19, t.dim);
    }
    yield;
    batch.build(c);
  } finally {
    batch.discard();
  }
}

/**
 * Port pills on the window border at the callsite line (out, right edge) or definition line (in,
 * left edge), labelled with the other end. Several links from one line share a pill (×N).
 */
export interface PortHit {
  x: number;
  y: number;
  w: number;
  h: number;
  window: WindowHeader;
  side: "in" | "out";
  /** The line the pill belongs to (the first, when neighbouring lines were merged into it). */
  line: number;
  /** Every line whose links the pill carries. */
  lines: number[];
  label: string;
  count: number;
}

type Port = Outline["ports"][number];

/** One pill being placed: its lines, links and where it ends up. */
interface Pill {
  side: "in" | "out";
  line: number;
  lines: number[];
  ports: Port[];
  /** Centre y on its line, and where it is drawn (lower when stacked under a neighbour). */
  cy0: number;
  cy: number;
}

function* buildPorts(c: Container, ws: WindowHeader[], look: Look, outlines: (id: string) => Outline | undefined, hits: PortHit[], stats: ChunkStats): Generator<void, void, void> {
  const t = look.theme;
  const font = monoFont();
  const rb = new RectBatch(ws.length * 8);
  const labels = new LabelBatch(font);
  try {
    // L2 pills are sized to read at outline zoom; L3 pills fit one code line.
    const size = look.tierIndex >= 3 ? 14 : 48;
    const h = look.tierIndex >= 3 ? 18 : 62;
    const pad = size * 0.6;
    // Pills taller than a line (L2) would overlap on neighbouring lines (N14): each one is stacked
    // just under the one above it, up to `maxShift` off its line (a leader marks where it belongs);
    // past that it merges into the one above as "label ×N".
    const gap = h * 0.12;
    const maxShift = h * 1.6;
    const px = 1 / look.strokeZoom;
    stats.pills = stats.stacked = stats.merged = 0;
    for (const w of ws) {
      yield;
      const o = outlines(w.id);
      if (!o || isMinimized(w, look)) continue;
      const byLine = new Map<string, Pill>();
      for (const p of o.ports) {
        if (look.hidden.has(GROUP_OF[p.kind])) continue;
        const k = p.side + p.line;
        const l = byLine.get(k);
        if (l) l.ports.push(p);
        else {
          const cy0 = w.rect.y + HEADER_WU + (p.line - w.lines.start + 0.5) * LINE_WU;
          byLine.set(k, { side: p.side as "in" | "out", line: p.line, lines: [p.line], ports: [p], cy0, cy: cy0 });
        }
      }
      const bottom = w.rect.y + w.rect.h + h / 2;
      for (const side of ["in", "out"] as const) {
        const list = [...byLine.values()].filter((p) => p.side === side).sort((a, b) => a.line - b.line);
        const placed: Pill[] = [];
        for (const p of list) {
          const last = placed[placed.length - 1];
          if (last && p.cy0 - h / 2 < last.cy + h / 2 + gap) {
            const cy = last.cy + h + gap;
            if (cy - p.cy0 <= maxShift && cy + h / 2 <= bottom) {
              p.cy = cy;
              stats.stacked++;
            } else {
              last.ports.push(...p.ports);
              last.lines.push(p.line);
              stats.merged++;
              continue;
            }
          }
          placed.push(p);
        }
        for (const p of placed) drawPill(p, w);
      }
    }

    function drawPill(pill: Pill, w: WindowHeader) {
      const p = pill.ports[0];
      const n = pill.ports.length;
      const text = fit(font, n > 1 ? `${p.label} ×${n}` : p.label, size, 360);
      const tw = textWidth(font, text, size) + 2 * pad;
      const cy = pill.cy;
      const x = pill.side === "out" ? w.rect.x + w.rect.w - 6 : w.rect.x + 6 - tw;
      const color = edgeColor(t, p.kind);
      stats.pills++;
      if (cy !== pill.cy0) {
        // A leader along the border from the pill's own line down to where it was stacked.
        const lx = pill.side === "out" ? w.rect.x + w.rect.w - 6 + Math.min(tw, h) / 2 : w.rect.x + 6 - Math.min(tw, h) / 2;
        const lw = Math.max(1.5 * px, h / 20);
        rb.add(lx - lw / 2, pill.cy0, lw, cy - pill.cy0, color, 0.7);
        rb.round(lx - 2.5 * lw, pill.cy0 - 2.5 * lw, 5 * lw, 5 * lw, 2.5 * lw, color, 0.9);
      }
      // Runtime-only targets (D11): no guessed link, an open "?" stub with what is known.
      let dyn: { dynamic: { hint: string; partial?: string } } | undefined;
      for (const q of pill.ports) {
        for (const id of q.links) {
          const to = look.linkOf?.(id)?.to;
          if (to && "dynamic" in to) {
            dyn = to as { dynamic: { hint: string; partial?: string } };
            break;
          }
        }
        if (dyn) break;
      }
      if (dyn) {
        const len = 60 * px;
        const sx = pill.side === "out" ? x + tw : x - len;
        dashedPolyline(rb, new Float32Array([sx, cy, sx + len, cy]), 5 * px, 4 * px, 1.5 * px, color, 0.9);
        const r = 9 * px;
        const ox = pill.side === "out" ? sx + len + r : sx - r;
        rb.round(ox - r, cy - r, 2 * r, 2 * r, r, color);
        rb.round(ox - r + 1.5 * px, cy - r + 1.5 * px, 2 * r - 3 * px, 2 * r - 3 * px, r - 1.5 * px, t.body);
        labels.add("?", ox - r * 0.35, cy - r * 0.8, r * 1.5, color);
        const stub = stubText(dyn.dynamic);
        if (drawnStubs.size < 200) drawnStubs.add(stub);
        labels.add(fit(font, stub, size * 0.9, 420), ox + r * 1.6, cy - size * 0.62, size * 0.9, t.dim);
      }
      hits.push({ x, y: cy - h / 2, w: tw, h, window: w, side: pill.side, line: pill.line, lines: pill.lines, label: p.label, count: n });
      // A rounded pill: a thin coloured rim around a lightly tinted fill.
      const bw = Math.max(1.2, h / 18);
      rb.round(x, cy - h / 2, tw, h, h / 2, color, 0.85);
      rb.round(x + bw, cy - h / 2 + bw, tw - 2 * bw, h - 2 * bw, h / 2 - bw, mix(t.body, color, t.name === "dark" ? 0.14 : 0.08));
      labels.add(text, x + pad, cy - size * 0.62, size, color);
    }

    yield;
    const pills = new Container();
    rb.build(pills);
    const text = new Container();
    labels.build(text);
    for (const ch of c.removeChildren()) dispose(ch);
    c.addChild(pills, text);
  } finally {
    rb.discard();
    labels.discard();
  }
}

/** Stub labels drawn by the last port builds (bounded; e2e checks read them). */
export const drawnStubs = new Set<string>();

/** A dynamic link's open stub (D11): "⇢ ? " and what is known of the runtime key. */
export function stubText(d: { hint: string; partial?: string }) {
  return `⇢ ? ${d.partial ? `${d.partial}…` : d.hint}`;
}

/**
 * L1: 1 px lines; from L2 links get width, arrowheads and markers. Both are quads in one mesh: typed
 * arrays only, where pixi Graphics hairlines built JS paths and tessellation garbage on every
 * rebuild (most of the GC load in long sessions on dense repos).
 */
function* buildChunkEdges(c: Container, edges: Edge[], look: Look, stats: ChunkStats): Generator<void, void, void> {
  const detail = look.tierIndex >= 2;
  const labels = detail ? new LabelBatch(sansFont()) : undefined;
  stats.lowMarks = stats.fadedLinks = 0;
  stats.maxEdgePx = 0;
  const es = { lowMarks: 0, maxWidthPx: 0, faded: 0 };
  // A lens (or the Hotspots brush) steps back the links between windows it fades, so its emphasis
  // is not drawn over by them: both ends out, almost gone; one end out, halfway.
  let fade: ((e: Edge) => number) | undefined;
  const windowOf = look.windowOf;
  if ((look.lens !== "none" || look.brush) && windowOf) {
    fade = (e) => {
      const a = windowOf(e.from);
      const b = windowOf(e.to);
      const n = (a && outOfFocus(a, look) ? 1 : 0) + (b && outOfFocus(b, look) ? 1 : 0);
      return n === 2 ? 0.18 : n === 1 ? 0.55 : 1;
    };
  }
  // L1: only links we are sure of; low-confidence ones (lexical candidates) come in at L2, faded with "?".
  const o = { theme: look.theme, wuPerPx: 1 / look.strokeZoom, detail, widthPx: detail ? 1.4 : 1, alpha: 0.8, hidden: look.hidden, labels, minConfidence: detail ? 0 : LOW_CONFIDENCE, bare: !detail, fade, stats: es };
  const rb = new RectBatch(edges.length * (detail ? 40 : 24));
  try {
    yield* quadEdgeSteps(rb, edges, o);
    stats.lowMarks = es.lowMarks;
    stats.maxEdgePx = es.maxWidthPx;
    stats.fadedLinks = es.faded;
    yield;
    // The old links stay on screen until the new ones are ready.
    for (const ch of c.removeChildren()) dispose(ch);
    const q = new Container();
    rb.build(q);
    c.addChild(q);
    if (labels) {
      const l = new Container();
      labels.build(l);
      c.addChild(l);
    }
  } finally {
    rb.discard();
    labels?.discard();
  }
}
