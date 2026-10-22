import type { Graphics } from "pixi.js";
import type { LinkKind } from "../contracts/model";
import type { Edge } from "../data/client";
import type { LabelBatch } from "./labels";
import type { RectBatch } from "./rects";
import type { Theme } from "./theme";

/** Link filters group kinds the way the legend does (DESIGN §5 "Edges"). */
export type LinkGroup = "calls" | "inherit" | "events" | "http" | "config";
export const LINK_GROUPS: LinkGroup[] = ["calls", "inherit", "events", "http", "config"];

export const GROUP_OF: Record<LinkKind, LinkGroup> = {
  call: "calls",
  reference: "calls",
  import: "calls",
  inherit: "inherit",
  implement: "inherit",
  publish: "events",
  subscribe: "events",
  dispatch: "events",
  ipc: "events",
  "http-request": "http",
  "http-route": "http",
  config: "config",
  define: "config",
  read: "config",
  deploy: "config",
};

interface GroupStyle {
  /** Dash on/off in screen px; undefined = solid. */
  dash?: [number, number];
  arrow: "filled" | "hollow";
  /** Mid-link channel marker: a topic diamond (events) or an endpoint pill (HTTP). */
  marker: "none" | "diamond" | "pill";
}

const STYLE: Record<LinkGroup, GroupStyle> = {
  calls: { arrow: "filled", marker: "none" },
  inherit: { dash: [8, 5], arrow: "hollow", marker: "none" },
  events: { arrow: "filled", marker: "diamond" },
  http: { arrow: "filled", marker: "pill" },
  config: { dash: [2, 4], arrow: "filled", marker: "none" },
};

/**
 * Kinds that share a legend group but not a look (DESIGN §5 "Edges"): deploy wiring crosses lanes
 * in long dashes, config keys stay dotted.
 */
const KIND_STYLE: Partial<Record<LinkKind, GroupStyle>> = {
  deploy: { dash: [11, 4], arrow: "filled", marker: "none" },
};

export function styleOf(kind: LinkKind): GroupStyle {
  return KIND_STYLE[kind] ?? STYLE[GROUP_OF[kind]];
}

export function edgeColor(t: Theme, kind: LinkKind) {
  return t.edge[kind] ?? t.edge.call;
}

/**
 * The curve of a link, flattened once and cached: out of the right edge at the callsite, into the
 * left edge at the definition, with horizontal tangents. Links that point backwards swing out wide
 * instead of folding over themselves. The point count grows with length so long curves stay smooth.
 */
export function edgePoints(e: Edge): Float32Array {
  if (e.pts) return e.pts;
  e.pts = curve(e.ax, e.ay, e.bx, e.by);
  return e.pts;
}

export function curve(ax: number, ay: number, bx: number, by: number, maxPts = 72): Float32Array {
  const fwd = bx - ax;
  const dy = Math.abs(by - ay);
  const dx = fwd > 80 ? Math.max(120, fwd * 0.5) : Math.max(360, -fwd * 0.4 + dy * 0.25);
  const len = Math.hypot(fwd, dy) + dx;
  const n = Math.max(12, Math.min(maxPts, Math.round(len / 90)));
  const out = new Float32Array((n + 1) * 2);
  for (let i = 0; i <= n; i++) {
    const t = i / n;
    const u = 1 - t;
    out[i * 2] = u * u * u * ax + 3 * u * u * t * (ax + dx) + 3 * u * t * t * (bx - dx) + t * t * t * bx;
    out[i * 2 + 1] = u * u * u * ay + 3 * u * u * t * ay + 3 * u * t * t * by + t * t * t * by;
  }
  return out;
}

export interface EdgeDraw {
  theme: Theme;
  /** World units per screen px at the zoom this geometry is sized for. */
  wuPerPx: number;
  /** 1 px hairlines (cheap, any zoom) or real width with arrows and markers. */
  detail: boolean;
  widthPx: number;
  alpha: number;
  hidden: ReadonlySet<LinkGroup>;
  /** Glyphs for the "?" on low-confidence links (detail mode); omitted, they get a ring only. */
  labels?: LabelBatch;
  /** Links below this confidence are left out (zoomed out, lexical candidates would swamp the map). */
  minConfidence?: number;
  /** Lines only: no arrowheads, channel markers or "?" (L1, where links are 1 px wide). */
  bare?: boolean;
  /** A lens stepping a link back: its alpha multiplier (1 keeps it). */
  fade?: (e: Edge) => number;
  /** Counters for tests: "?" markers drawn, widest link (px), links stepped back by `fade`. */
  stats?: { lowMarks: number; maxWidthPx: number; faded?: number };
}

/** Below this a link is drawn faded with "?" (Link.confidence). */
export const LOW_CONFIDENCE = 0.6;

/**
 * Config-field links (D15, DESIGN §4.6) are solid from this score up and faded with "?" below it
 * (they are kept from 0.65): cv-channel-config `SOLID_SCORE`, which a Rust test keeps equal.
 */
export const CONFIG_SOLID_SCORE = 0.8;

/** Whether a link is drawn as uncertain (faded, dashed, with "?"): its kind's threshold. */
export function isUncertain(e: { kind: LinkKind; confidence: number }): boolean {
  return e.confidence < (GROUP_OF[e.kind] === "config" ? CONFIG_SOLID_SCORE : LOW_CONFIDENCE);
}

/** Most dashes per line: a long link at code zoom would otherwise be tens of thousands of dashes. */
const MAX_DASHES = 160;

/** Dash and gap for `pts`, stretched so the line has at most MAX_DASHES dashes. */
function dashFor(pts: Float32Array, on: number, off: number): [number, number] {
  let len = 0;
  for (let i = 2; i < pts.length; i += 2) {
    const dx = pts[i] - pts[i - 2];
    const dy = pts[i + 1] - pts[i - 1];
    len += Math.sqrt(dx * dx + dy * dy);
  }
  const k = len / ((on + off) * MAX_DASHES);
  return k > 1 ? [on * k, off * k] : [on, off];
}

/** Trace `pts` into `g` as one sub-path, or as dashes of `dash` (world units). */
function trace(g: Graphics, pts: Float32Array, dash?: [number, number]) {
  if (!dash) {
    g.moveTo(pts[0], pts[1]);
    for (let i = 2; i < pts.length; i += 2) g.lineTo(pts[i], pts[i + 1]);
    return;
  }
  const [on, off] = dashFor(pts, dash[0], dash[1]);
  let drawing = true;
  let left = on;
  let x = pts[0];
  let y = pts[1];
  g.moveTo(x, y);
  for (let i = 2; i < pts.length; i += 2) {
    const nx = pts[i];
    const ny = pts[i + 1];
    let seg = Math.sqrt((nx - x) * (nx - x) + (ny - y) * (ny - y));
    while (seg > 0) {
      const step = Math.min(seg, left);
      const f = step / seg;
      x += (nx - x) * f;
      y += (ny - y) * f;
      seg -= step;
      left -= step;
      if (drawing) g.lineTo(x, y);
      else g.moveTo(x, y);
      if (left <= 1e-6) {
        drawing = !drawing;
        left = drawing ? on : off;
      }
    }
  }
}

function styled(edges: Edge[], hidden: ReadonlySet<LinkGroup>, minConfidence = 0) {
  const byStyle = new Map<string, Edge[]>();
  for (const e of edges) {
    if (hidden.has(GROUP_OF[e.kind]) || e.confidence < minConfidence) continue;
    const k = e.kind + (isUncertain(e) ? "?" : "");
    const l = byStyle.get(k);
    if (l) l.push(e);
    else byStyle.set(k, [e]);
  }
  return [...byStyle].map(([k, list]) => {
    const low = k.endsWith("?");
    const kind = (low ? k.slice(0, -1) : k) as LinkKind;
    const style = styleOf(kind);
    // Low confidence is always dashed, so it reads as uncertain even without colour.
    const dash = style.dash ?? (low ? ([5, 4] as [number, number]) : undefined);
    return { kind, low, style, dash, list };
  });
}

/** Hairlines (1 px at any zoom), coloured and dashed by kind: cheap enough for every link at L1. */
export function drawEdges(g: Graphics, edges: Edge[], o: EdgeDraw) {
  const px = o.wuPerPx;
  for (const { kind, low, dash, list } of styled(edges, o.hidden, o.minConfidence)) {
    const dashWu = dash && ([dash[0] * px, dash[1] * px] as [number, number]);
    for (const e of list) trace(g, edgePoints(e), dashWu);
    g.stroke({ width: 1, pixelLine: true, color: edgeColor(o.theme, kind), alpha: o.alpha * (low ? 0.45 : 1) });
  }
}

/** Walk `pts` calling `seg` for each drawn piece of a dash pattern (world units). */
function dashes(pts: Float32Array, on0: number, off0: number, seg: (ax: number, ay: number, bx: number, by: number) => void) {
  const [on, off] = dashFor(pts, on0, off0);
  let drawing = true;
  let left = on;
  let x = pts[0];
  let y = pts[1];
  for (let i = 2; i < pts.length; i += 2) {
    const nx = pts[i];
    const ny = pts[i + 1];
    let rem = Math.sqrt((nx - x) * (nx - x) + (ny - y) * (ny - y));
    while (rem > 1e-9) {
      const step = Math.min(rem, left);
      const f = step / rem;
      const sx = x + (nx - x) * f;
      const sy = y + (ny - y) * f;
      if (drawing) seg(x, y, sx, sy);
      x = sx;
      y = sy;
      rem -= step;
      left -= step;
      if (left <= 1e-9) {
        drawing = !drawing;
        left = drawing ? on : off;
      }
    }
  }
}

const tmp = new Float32Array(4);
// One segment callback for every dashed line (no closure per line): its target is set per call.
let segRb: RectBatch;
let segW = 0;
let segColor = 0;
let segAlpha = 1;
function segToBatch(ax: number, ay: number, bx: number, by: number) {
  tmp[0] = ax;
  tmp[1] = ay;
  tmp[2] = bx;
  tmp[3] = by;
  segRb.polyline(tmp, segW, segColor, segAlpha);
}

/** A dashed polyline of world width `w` (dash lengths in world units) as quads. */
export function dashedPolyline(rb: RectBatch, pts: Float32Array, on: number, off: number, w: number, color: number, alpha: number) {
  segRb = rb;
  segW = w;
  segColor = color;
  segAlpha = alpha;
  dashes(pts, on, off, segToBatch);
}

function line(rb: RectBatch, ax: number, ay: number, bx: number, by: number, w: number, color: number, alpha: number) {
  tmp[0] = ax;
  tmp[1] = ay;
  tmp[2] = bx;
  tmp[3] = by;
  rb.polyline(tmp, w, color, alpha);
}

/**
 * Links with real width, arrowheads and channel markers, as quads in a RectBatch: no tessellation,
 * so a chunk's links build in about a millisecond.
 */
export function quadEdges(rb: RectBatch, edges: Edge[], o: EdgeDraw) {
  for (const _ of quadEdgeSteps(rb, edges, o, Infinity));
}

/** `quadEdges` as a resumable build: yields after every `every` links (chunk builds slice it across frames). */
export function* quadEdgeSteps(rb: RectBatch, edges: Edge[], o: EdgeDraw, every = 24): Generator<void, void, void> {
  const t = o.theme;
  let k = 0;
  const px = o.wuPerPx;
  const w = o.widthPx * px;
  const stats = o.stats;
  for (const { kind, low, style, dash, list } of styled(edges, o.hidden, o.minConfidence)) {
    const color = edgeColor(t, kind);
    const alpha0 = o.alpha * (low ? 0.45 : 1);
    const size0 = 7 * px * Math.max(1, o.widthPx / 1.5);
    for (const e of list) {
      if (++k >= every) {
        k = 0;
        yield;
      }
      const p = edgePoints(e);
      const fade = o.fade ? o.fade(e) : 1;
      const alpha = alpha0 * fade;
      const size = size0;
      if (stats) {
        if (o.widthPx > stats.maxWidthPx) stats.maxWidthPx = o.widthPx;
        if (fade < 1) stats.faded = (stats.faded ?? 0) + 1;
      }
      if (dash) dashedPolyline(rb, p, dash[0] * px, dash[1] * px, w, color, alpha);
      else if (o.detail) taperedPolyline(rb, p, w * 1.5, w * 0.75, color, alpha);
      else rb.polyline(p, w, color, alpha);
      if (o.bare) continue;
      // Arrowhead at the in port.
      const n = p.length;
      const ex = p[n - 2];
      const ey = p[n - 1];
      let ux = ex - p[n - 4];
      let uy = ey - p[n - 3];
      const ul = Math.sqrt(ux * ux + uy * uy) || 1;
      ux /= ul;
      uy /= ul;
      const bx = ex - ux * size;
      const by = ey - uy * size;
      const lx = bx - uy * size * 0.55;
      const ly = by + ux * size * 0.55;
      const rx = bx + uy * size * 0.55;
      const ry = by - ux * size * 0.55;
      // Filled arrows are notched (a concave kite), which reads lighter than a solid triangle.
      if (style.arrow === "filled") rb.quad(ex, ey, lx, ly, ex - ux * size * 0.68, ey - uy * size * 0.68, rx, ry, color, alpha);
      else {
        rb.quad(ex, ey, lx, ly, rx, ry, rx, ry, t.bg, alpha);
        line(rb, ex, ey, lx, ly, 1.2 * px, color, alpha);
        line(rb, lx, ly, rx, ry, 1.2 * px, color, alpha);
        line(rb, rx, ry, ex, ey, 1.2 * px, color, alpha);
      }
      // Channel marker mid-way: a topic diamond for events, an amber endpoint pill for HTTP.
      if (style.marker !== "none") {
        const m = (p.length / 4) | 0;
        const mx = p[m * 2];
        const my = p[m * 2 + 1];
        const r = 5 * px;
        if (style.marker === "diamond") {
          rb.quad(mx, my - r, mx + r, my, mx, my + r, mx - r, my, color, alpha);
          const q = r * 0.55;
          rb.quad(mx, my - q, mx + q, my, mx, my + q, mx - q, my, t.bg, 1);
        } else {
          const pw = 3.4 * r;
          const ph = 1.7 * r;
          const b = 1.3 * px;
          rb.round(mx - pw / 2, my - ph / 2, pw, ph, ph / 2, color, alpha);
          rb.round(mx - pw / 2 + b, my - ph / 2 + b, pw - 2 * b, ph - 2 * b, ph / 2 - b, t.bg, 1);
          rb.add(mx - pw * 0.22, my - 0.6 * px, pw * 0.44, 1.2 * px, color, alpha);
        }
      }
      // Low confidence: a "?" in a small disc two thirds along, so it reads without colour.
      if (low) {
        const m = ((p.length / 2) * 0.66) | 0;
        const mx = p[m * 2];
        const my = p[m * 2 + 1];
        const r = 6 * px;
        rb.round(mx - r, my - r, 2 * r, 2 * r, r, color, Math.min(1, alpha * 1.8));
        rb.round(mx - r + 1.2 * px, my - r + 1.2 * px, 2 * r - 2.4 * px, 2 * r - 2.4 * px, r - 1.2 * px, t.bg, 1);
        o.labels?.add("?", mx - r * 0.32, my - r * 0.78, r * 1.5, color);
        if (stats) stats.lowMarks++;
      }
    }
  }
}

/**
 * A polyline whose width runs from `w0` at its start to `w1` at its end: links taper from the
 * call site to the definition, so direction reads without looking for the arrowhead (and a bundle
 * of links into one definition narrows instead of piling up). Same quads as `polyline`.
 */
export function taperedPolyline(rb: RectBatch, pts: Float32Array, w0: number, w1: number, color: number, alpha = 1) {
  const n = pts.length / 2 - 1;
  for (let i = 1; i <= n; i++) {
    const ax = pts[i * 2 - 2];
    const ay = pts[i * 2 - 1];
    const bx = pts[i * 2];
    const by = pts[i * 2 + 1];
    const ha = (w0 + ((w1 - w0) * (i - 1)) / n) / 2;
    const hb = (w0 + ((w1 - w0) * i) / n) / 2;
    const dx = bx - ax;
    const dy = by - ay;
    const l = Math.sqrt(dx * dx + dy * dy) || 1;
    const ux = dx / l;
    const uy = dy / l;
    // Each segment reaches back by half its width to cover the joint with the previous one.
    const ex = i === 1 ? 0 : ux * ha;
    const ey = i === 1 ? 0 : uy * ha;
    rb.quad(ax - ex - uy * ha, ay - ey + ux * ha, bx + ux * hb - uy * hb, by + uy * hb + ux * hb, bx + ux * hb + uy * hb, by + uy * hb - ux * hb, ax - ex + uy * ha, ay - ey - ux * ha, color, alpha);
  }
}

/** Squared distance from (x, y) to the polyline `pts`. */
export function distSq(pts: Float32Array, x: number, y: number) {
  let best = Infinity;
  for (let i = 2; i < pts.length; i += 2) {
    const ax = pts[i - 2];
    const ay = pts[i - 1];
    const dx = pts[i] - ax;
    const dy = pts[i + 1] - ay;
    const l2 = dx * dx + dy * dy;
    const t = l2 ? Math.max(0, Math.min(1, ((x - ax) * dx + (y - ay) * dy) / l2)) : 0;
    const qx = ax + dx * t - x;
    const qy = ay + dy * t - y;
    const d = qx * qx + qy * qy;
    if (d < best) best = d;
  }
  return best;
}
