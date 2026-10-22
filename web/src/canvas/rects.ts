import { CanvasSource, Container, Graphics, Mesh, MeshGeometry, Texture } from "pixi.js";

/**
 * Destroy a display object and what it owns. Pixi's `destroy({ children: true })` keeps a Graphics'
 * own context (its paths and GPU data stay registered with the renderer) and a Mesh's geometry (its
 * buffers and typed arrays): every rebuild leaked both. `Geometry.destroy()` alone still leaves its
 * buffers in the renderer's managed set (GPU buffer and vertex data) until the resource GC runs
 * minutes later, so the buffers are destroyed too. Shared textures (palette, glyph atlases) stay.
 */
export function dispose(o: Container) {
  for (const ch of o.removeChildren()) dispose(ch);
  if (o instanceof Graphics) return o.destroy({ context: true });
  if (o instanceof Mesh) {
    const g = o.geometry;
    o.destroy();
    g?.destroy(true);
    return;
  }
  o.destroy();
}

/**
 * Axis-aligned rectangles as one mesh, coloured through a shared palette texture: each quad's UVs
 * point at its colour's texels. Building is a typed-array fill with no per-shape objects, so large
 * chunks build in well under a millisecond and leave almost nothing for the GC.
 *
 * The palette is an atlas of 16×16 cells, one per (colour, alpha, shape). Every cell holds a
 * quarter shape in its top-left with solid colour toward the bottom-right corner: a disc (rounded
 * corners), a soft falloff (shadows and glows) or a thin ring (outlines). Plain quads sample the
 * cell's solid corner texel, so rounded cards, pills and shadows stay in the same single mesh as
 * everything else: a corner is just one more quad with different UVs.
 */

const CELL = 16;
const SIZE = 512;
const PER_ROW = SIZE / CELL;
/** Shape radius in texels: from the cell's outer texel edge (1) to the centre of its solid corner texel. */
const R = CELL - 1.5;
/** Ring thickness in texels: a ring corner of radius r draws a line r·RING/R thick. */
const RING = 2.5;
const canvas = document.createElement("canvas");
canvas.width = SIZE;
canvas.height = SIZE;
const ctx = canvas.getContext("2d")!;
const source = new CanvasSource({
  resource: canvas,
  scaleMode: "linear",
  autoGenerateMipmaps: true,
  mipmapFilter: "linear",
});
const paletteTexture = new Texture({ source });
/**
 * Atlas cell + 1 per (alpha/64, shape), per colour: integer keys only, so lookups allocate nothing
 * (a combined numeric key overflowed small integers and boxed a heap number per lookup).
 */
const slots = new Map<number, Int32Array>();
let slotCount = 0;
/** Per cell: its outer edge (shape boundary) u, v and its solid corner texel centre u, v, computed once. */
const uvs = new Float32Array(PER_ROW * PER_ROW * 4);
let dirty = false;

const enum Shape {
  Disc = 0,
  Soft = 1,
  Ring = 2,
}

function coverage(d: number, r: number) {
  return Math.min(1, Math.max(0, r - d + 0.5));
}

// Builds draw runs of quads in one colour: remember the last lookup (integers only).
let lastColor = -1;
let lastA64 = -1;
let lastShape = -1;
let lastSlot = 0;

function slot(color: number, alpha: number, shape: Shape): number {
  // Animated alphas (presence fades) would otherwise mint a cell, and an atlas upload, per frame.
  const a64 = alpha >= 1 ? 64 : alpha <= 0 ? 0 : (alpha * 64 + 0.5) | 0;
  if (color === lastColor && a64 === lastA64 && shape === lastShape) return lastSlot;
  lastColor = color;
  lastA64 = a64;
  lastShape = shape;
  return (lastSlot = slotOf(color & 0xffffff, a64, shape));
}

function slotOf(color: number, a64: number, shape: Shape): number {
  let row = slots.get(color);
  if (!row) slots.set(color, (row = new Int32Array(65 * 3)));
  let i = row[a64 * 3 + shape] - 1;
  if (i < 0) {
    if (slotCount >= PER_ROW * PER_ROW) {
      // Full: borrow the nearest colour's cell rather than fail the build (a tint comes out a
      // little off). Draw code keeps colours to fixed steps (theme.ts `step`) so this stays rare.
      i = nearestSlot(color, a64, shape);
      row[a64 * 3 + shape] = i + 1;
      if (!warnedFull) console.warn("rect palette full: reusing the nearest colours");
      warnedFull = true;
      return i;
    }
    i = slotCount++;
    row[a64 * 3 + shape] = i + 1;
    const alpha = a64 / 64;
    const x0 = (i % PER_ROW) * CELL;
    const y0 = ((i / PER_ROW) | 0) * CELL;
    uvs.set([(x0 + 1) / SIZE, (y0 + 1) / SIZE, (x0 + CELL - 0.5) / SIZE, (y0 + CELL - 0.5) / SIZE], i * 4);
    const img = ctx.createImageData(CELL, CELL);
    const d = img.data;
    const r = (color >> 16) & 255;
    const g = (color >> 8) & 255;
    const b = color & 255;
    const c = CELL - 0.5;
    for (let y = 0; y < CELL; y++) {
      for (let x = 0; x < CELL; x++) {
        const dist = Math.hypot(c - (x + 0.5), c - (y + 0.5));
        let a: number;
        if (shape === Shape.Disc) a = coverage(dist, R);
        else if (shape === Shape.Ring) a = coverage(dist, R) - coverage(dist, R - RING);
        else {
          // Smoothstep falloff from the solid corner to the outer edge: a cheap Gaussian look-alike.
          const t = Math.min(1, dist / R);
          a = (1 - t) * (1 - t) * (1 + 2 * t);
        }
        const o = (y * CELL + x) * 4;
        d[o] = r;
        d[o + 1] = g;
        d[o + 2] = b;
        d[o + 3] = Math.round(a * alpha * 255);
      }
    }
    ctx.putImageData(img, (i % PER_ROW) * CELL, ((i / PER_ROW) | 0) * CELL);
    dirty = true;
  }
  return i;
}

/** Palette cells in use and the atlas capacity (a colour × alpha × shape each; never freed). */
export function paletteUse() {
  return { cells: slotCount, uploads, capacity: PER_ROW * PER_ROW };
}
let uploads = 0;

let warnedFull = false;

/** The cell of the existing colour nearest `color` with this alpha and shape (else any alpha). */
function nearestSlot(color: number, a64: number, shape: Shape): number {
  const r = (color >> 16) & 255;
  const g = (color >> 8) & 255;
  const b = color & 255;
  let best = 0;
  let bestD = Infinity;
  for (const exact of [true, false]) {
    for (const [c, row] of slots) {
      const dr = ((c >> 16) & 255) - r;
      const dg = ((c >> 8) & 255) - g;
      const db = (c & 255) - b;
      const dc = dr * dr + dg * dg + db * db;
      for (let a = exact ? a64 : 0; a <= (exact ? a64 : 64); a++) {
        const j = row[a * 3 + shape] - 1;
        if (j < 0) continue;
        const d = dc + (a - a64) * (a - a64) * 64;
        if (d < bestD) {
          bestD = d;
          best = j;
        }
      }
    }
    if (bestD < Infinity) break;
  }
  return best;
}

/** Call before rendering: uploads the palette if new colours were added. */
export function flushPalette() {
  if (dirty) {
    uploads++;
    source.update();
    dirty = false;
  }
}

/** Corner flags for `RectBatch.round`. */
export const TL = 1;
export const TR = 2;
export const BR = 4;
export const BL = 8;
export const ALL = 15;

/**
 * Scratch vertex arrays shared by every batch being filled: a batch borrows a pair on construction
 * and hands it back in `build`, where the mesh gets exact-size copies. Builds then allocate only
 * what the mesh keeps (no capacity guesses, no doubling garbage), and a mesh never pins an
 * over-allocated buffer through a subarray: that retained most of a long session's heap.
 */
interface Scratch {
  pos: Float32Array;
  uv: Float32Array;
}
const scratchPool: Scratch[] = [];
const EMPTY = new Float32Array(0);

function borrow(capacity: number): Scratch {
  const s = scratchPool.pop() ?? { pos: new Float32Array(0), uv: new Float32Array(0) };
  if (s.pos.length < capacity * 8) {
    const len = Math.max(capacity * 8, 4096);
    s.pos = new Float32Array(len);
    s.uv = new Float32Array(len);
  }
  return s;
}

/** Quad indices (0 1 2, 0 2 3 per quad), shared by every mesh as subarrays: they never change. */
let quadIndices = new Uint32Array(0);

/** Indices for `n` quads (a view of the shared array, grown on demand). */
export function indicesFor(n: number): Uint32Array {
  if (quadIndices.length < n * 6) {
    const idx = new Uint32Array(Math.max(n * 6, quadIndices.length * 2, 6 * 4096));
    for (let i = 0, v = 0; i < idx.length; i += 6, v += 4) {
      idx[i] = v;
      idx[i + 1] = v + 1;
      idx[i + 2] = v + 2;
      idx[i + 3] = v;
      idx[i + 4] = v + 2;
      idx[i + 5] = v + 3;
    }
    quadIndices = idx;
  }
  return quadIndices.subarray(0, n * 6);
}

export class RectBatch {
  private s: Scratch;
  private pos: Float32Array;
  private uv: Float32Array;
  private n = 0;
  private spent = false;

  /** `capacity`: a hint in quads; the batch grows as needed. */
  constructor(capacity = 256) {
    this.s = borrow(capacity);
    this.pos = this.s.pos;
    this.uv = this.s.uv;
  }

  /** Quads added so far. */
  get size() {
    return this.n;
  }

  private grow() {
    if (this.spent) {
      // Filled again after a build: borrow a fresh pair (the old one may be in use elsewhere).
      this.spent = false;
      this.s = borrow(256);
      this.pos = this.s.pos;
      this.uv = this.s.uv;
      return;
    }
    const pos = new Float32Array(this.pos.length * 2);
    pos.set(this.pos);
    this.pos = this.s.pos = pos;
    const uv = new Float32Array(this.uv.length * 2);
    uv.set(this.uv);
    this.uv = this.s.uv = uv;
  }

  add(x: number, y: number, w: number, h: number, color: number, alpha = 1) {
    if (w <= 0 || h <= 0) return;
    if ((this.n + 1) * 8 > this.pos.length) this.grow();
    const o = this.n * 8;
    const p = this.pos;
    p[o] = x;
    p[o + 1] = y;
    p[o + 2] = x + w;
    p[o + 3] = y;
    p[o + 4] = x + w;
    p[o + 5] = y + h;
    p[o + 6] = x;
    p[o + 7] = y + h;
    const si = slot(color, alpha, Shape.Disc) * 4;
    const u = uvs[si + 2];
    const v = uvs[si + 3];
    const q = this.uv;
    for (let k = 0; k < 8; k += 2) {
      q[o + k] = u;
      q[o + k + 1] = v;
    }
    this.n++;
  }

  /** Any quad (corners in order), e.g. one segment of a thick line. */
  quad(x0: number, y0: number, x1: number, y1: number, x2: number, y2: number, x3: number, y3: number, color: number, alpha = 1) {
    if ((this.n + 1) * 8 > this.pos.length) this.grow();
    const o = this.n * 8;
    const p = this.pos;
    p[o] = x0;
    p[o + 1] = y0;
    p[o + 2] = x1;
    p[o + 3] = y1;
    p[o + 4] = x2;
    p[o + 5] = y2;
    p[o + 6] = x3;
    p[o + 7] = y3;
    const si = slot(color, alpha, Shape.Disc) * 4;
    const u = uvs[si + 2];
    const v = uvs[si + 3];
    const q = this.uv;
    for (let k = 0; k < 8; k += 2) {
      q[o + k] = u;
      q[o + k + 1] = v;
    }
    this.n++;
  }

  /**
   * A polyline of width `w` as one quad per segment, each stretched by half the width at both ends
   * so consecutive segments overlap at the joints (no tessellation, no join geometry). Translucent
   * lines show slightly denser joints; that is the price of building in microseconds.
   */
  polyline(pts: Float32Array, w: number, color: number, alpha = 1) {
    const h = w / 2;
    for (let i = 2; i < pts.length; i += 2) {
      const ax = pts[i - 2];
      const ay = pts[i - 1];
      const bx = pts[i];
      const by = pts[i + 1];
      const dx = bx - ax;
      const dy = by - ay;
      const l = Math.sqrt(dx * dx + dy * dy) || 1;
      const ux = ((bx - ax) / l) * h;
      const uy = ((by - ay) / l) * h;
      const ex = i === 2 ? 0 : ux;
      const ey = i === 2 ? 0 : uy;
      this.quad(ax - ex - uy, ay - ey + ux, bx + ux - uy, by + uy + ux, bx + ux + uy, by + uy - ux, ax - ex + uy, ay - ey - ux, color, alpha);
    }
  }

  /**
   * A quad whose UVs run from a cell's outer edge (at the first two corners' outer side) to its solid
   * corner: (ox, oy) is the outer corner of the shape, (ix, iy) the inner one.
   */
  private corner(ox: number, oy: number, ix: number, iy: number, si: number) {
    if ((this.n + 1) * 8 > this.pos.length) this.grow();
    const o = this.n * 8;
    const p = this.pos;
    const q = this.uv;
    const u0 = uvs[si], v0 = uvs[si + 1], u1 = uvs[si + 2], v1 = uvs[si + 3];
    p[o] = ox;
    p[o + 1] = oy;
    q[o] = u0;
    q[o + 1] = v0;
    p[o + 2] = ix;
    p[o + 3] = oy;
    q[o + 2] = u1;
    q[o + 3] = v0;
    p[o + 4] = ix;
    p[o + 5] = iy;
    q[o + 4] = u1;
    q[o + 5] = v1;
    p[o + 6] = ox;
    p[o + 7] = iy;
    q[o + 6] = u0;
    q[o + 7] = v1;
    this.n++;
  }

  /** An edge strip of a soft or ring shape: the profile runs across it from the outer side (o) inward (i). */
  private strip(x0: number, y0: number, x1: number, y1: number, horizontal: boolean, outerFirst: boolean, si: number) {
    if ((this.n + 1) * 8 > this.pos.length) this.grow();
    const o = this.n * 8;
    const p = this.pos;
    const q = this.uv;
    const u0 = uvs[si], v0 = uvs[si + 1], u1 = uvs[si + 2], v1 = uvs[si + 3];
    // Along the strip the UV stays on the solid column/row; across it, it walks the profile.
    const a = outerFirst ? (horizontal ? v0 : u0) : horizontal ? v1 : u1;
    const b = outerFirst ? (horizontal ? v1 : u1) : horizontal ? v0 : u0;
    p[o] = x0;
    p[o + 1] = y0;
    p[o + 2] = x1;
    p[o + 3] = y0;
    p[o + 4] = x1;
    p[o + 5] = y1;
    p[o + 6] = x0;
    p[o + 7] = y1;
    if (horizontal) {
      q[o] = u1;
      q[o + 1] = a;
      q[o + 2] = u1;
      q[o + 3] = a;
      q[o + 4] = u1;
      q[o + 5] = b;
      q[o + 6] = u1;
      q[o + 7] = b;
    } else {
      q[o] = a;
      q[o + 1] = v1;
      q[o + 2] = b;
      q[o + 3] = v1;
      q[o + 4] = b;
      q[o + 5] = v1;
      q[o + 6] = a;
      q[o + 7] = v1;
    }
    this.n++;
  }

  /** A rounded rectangle (radius `r`, clamped to fit) as seven quads; `corners` picks which are rounded. */
  round(x: number, y: number, w: number, h: number, r: number, color: number, alpha = 1, corners = ALL) {
    if (w <= 0 || h <= 0) return;
    r = Math.min(r, w / 2, h / 2);
    if (r <= 0 || !corners) return this.add(x, y, w, h, color, alpha);
    const uv = slot(color, alpha, Shape.Disc) * 4;
    const x1 = x + w;
    const y1 = y + h;
    // Square corners are plain quads (no closure: this runs for every card of every build).
    if (corners & TL) this.corner(x, y, x + r, y + r, uv);
    else this.add(x, y, r, r, color, alpha);
    if (corners & TR) this.corner(x1, y, x1 - r, y + r, uv);
    else this.add(x1 - r, y, r, r, color, alpha);
    if (corners & BR) this.corner(x1, y1, x1 - r, y1 - r, uv);
    else this.add(x1 - r, y1 - r, r, r, color, alpha);
    if (corners & BL) this.corner(x, y1, x + r, y1 - r, uv);
    else this.add(x, y1 - r, r, r, color, alpha);
    this.add(x + r, y, w - 2 * r, r, color, alpha);
    this.add(x, y + r, w, h - 2 * r, color, alpha);
    this.add(x + r, y1 - r, w - 2 * r, r, color, alpha);
  }

  /**
   * A soft shadow or glow around the box (x, y, w, h), fading out over `spread`. Only the eight
   * outer quads are drawn: the box itself is assumed covered by what casts the shadow.
   */
  soft(x: number, y: number, w: number, h: number, spread: number, color: number, alpha = 1, fillCore = false) {
    if (spread <= 0 || w <= 0 || h <= 0) return;
    const uv = slot(color, alpha, Shape.Soft) * 4;
    const x1 = x + w;
    const y1 = y + h;
    const s = spread;
    this.corner(x - s, y - s, x, y, uv);
    this.corner(x1 + s, y - s, x1, y, uv);
    this.corner(x1 + s, y1 + s, x1, y1, uv);
    this.corner(x - s, y1 + s, x, y1, uv);
    this.strip(x, y - s, x1, y, true, true, uv);
    this.strip(x, y1, x1, y1 + s, true, false, uv);
    this.strip(x - s, y, x, y1, false, true, uv);
    this.strip(x1, y, x1 + s, y1, false, false, uv);
    if (fillCore) this.add(x, y, w, h, color, alpha);
  }

  /** A rounded outline of width `bw` inside (x, y, w, h); its corner radius is fixed by the width (≈ 5.8·bw). */
  ring(x: number, y: number, w: number, h: number, bw: number, color: number, alpha = 1) {
    if (w <= 0 || h <= 0 || bw <= 0) return;
    const r = Math.min((bw * R) / RING, w / 2, h / 2);
    const uv = slot(color, alpha, Shape.Ring) * 4;
    const x1 = x + w;
    const y1 = y + h;
    this.corner(x, y, x + r, y + r, uv);
    this.corner(x1, y, x1 - r, y + r, uv);
    this.corner(x1, y1, x1 - r, y1 - r, uv);
    this.corner(x, y1, x + r, y1 - r, uv);
    this.add(x + r, y, w - 2 * r, bw, color, alpha);
    this.add(x + r, y1 - bw, w - 2 * r, bw, color, alpha);
    this.add(x, y + r, bw, h - 2 * r, color, alpha);
    this.add(x1 - bw, y + r, bw, h - 2 * r, color, alpha);
  }

  /** Border of width `bw` inside (x, y, w, h). */
  frame(x: number, y: number, w: number, h: number, bw: number, color: number, alpha = 1) {
    this.add(x, y, w, bw, color, alpha);
    this.add(x, y + h - bw, w, bw, color, alpha);
    this.add(x, y + bw, bw, h - 2 * bw, color, alpha);
    this.add(x + w - bw, y + bw, bw, h - 2 * bw, color, alpha);
  }

  /**
   * Replace `into`'s children with the built mesh (exact-size copies of the vertices; shared
   * indices). The batch is spent afterwards: its scratch arrays go back to the pool.
   */
  build(into: Container) {
    for (const ch of into.removeChildren()) dispose(ch);
    const n = this.n;
    this.n = 0;
    if (n) {
      const geometry = new MeshGeometry({
        positions: this.pos.slice(0, n * 8),
        uvs: this.uv.slice(0, n * 8),
        indices: indicesFor(n),
      });
      into.addChild(new Mesh({ geometry, texture: paletteTexture }));
    }
    if (!this.spent) scratchPool.push(this.s);
    this.spent = true;
    this.pos = this.uv = EMPTY;
  }

  /** Drop what was added without building it (an abandoned resumable build): the scratch goes back. */
  discard() {
    this.n = 0;
    if (!this.spent) scratchPool.push(this.s);
    this.spent = true;
    this.pos = this.uv = EMPTY;
  }
}
