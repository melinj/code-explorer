import { dispose, indicesFor } from "./rects";
import { CanvasSource, Container, Mesh, MeshGeometry, Texture } from "pixi.js";

/**
 * World-space labels as static meshes over a glyph atlas. Building a chunk's labels is a typed-array
 * fill (no text layout objects, no per-label scene nodes), and drawing them is one draw call per
 * colour. Glyphs are rasterized once at ATLAS_PX with mipmaps, so they stay legible from a few
 * pixels up to about twice that size.
 */

const ATLAS_PX = 48;
const PAD = 4;
const CHARS = Array.from({ length: 95 }, (_, i) => String.fromCharCode(32 + i)).join("") + "·…→←↑↓↧↥−×✓⇢";

export interface GlyphFont {
  texture: Texture;
  /** Per char: u0, v0, u1, v1, advance (atlas px), cell width (atlas px). */
  glyphs: Map<string, [number, number, number, number, number, number]>;
  cellH: number;
  ascent: number;
}

const fonts = new Map<string, GlyphFont>();

export function glyphFont(name: string, cssFont: (px: number) => string): GlyphFont {
  const have = fonts.get(name);
  if (have) return have;
  const canvas = document.createElement("canvas");
  const g = canvas.getContext("2d")!;
  g.font = cssFont(ATLAS_PX);
  const m = g.measureText("Mg");
  const ascent = Math.ceil(m.fontBoundingBoxAscent ?? ATLAS_PX * 0.8);
  const cellH = ascent + Math.ceil(m.fontBoundingBoxDescent ?? ATLAS_PX * 0.25) + 2 * PAD;
  const W = 1024;
  const cells: Array<[string, number, number, number, number]> = [];
  let x = 0;
  let y = 0;
  for (const ch of CHARS) {
    const adv = g.measureText(ch).width;
    const cw = Math.ceil(adv) + 2 * PAD;
    if (x + cw > W) {
      x = 0;
      y += cellH;
    }
    cells.push([ch, x, y, cw, adv]);
    x += cw;
  }
  canvas.width = W;
  canvas.height = 1 << Math.ceil(Math.log2(y + cellH));
  g.font = cssFont(ATLAS_PX);
  g.fillStyle = "#fff";
  g.textBaseline = "alphabetic";
  const glyphs: GlyphFont["glyphs"] = new Map();
  for (const [ch, cx, cy, cw, adv] of cells) {
    g.fillText(ch, cx + PAD, cy + PAD + ascent);
    glyphs.set(ch, [cx / W, cy / canvas.height, (cx + cw) / W, (cy + cellH) / canvas.height, adv, cw]);
  }
  const source = new CanvasSource({
    resource: canvas,
    autoGenerateMipmaps: true,
    scaleMode: "linear",
    mipmapFilter: "linear",
    maxAnisotropy: 4,
  });
  const font = { texture: new Texture({ source }), glyphs, cellH, ascent };
  fonts.set(name, font);
  return font;
}

export function textWidth(font: GlyphFont, text: string, size: number) {
  let w = 0;
  for (const ch of text) w += (font.glyphs.get(ch) ?? font.glyphs.get("?")!)[4];
  return (w * size) / ATLAS_PX;
}

/** Truncate `text` with an ellipsis so it fits `maxW` at `size`. */
export function fit(font: GlyphFont, text: string, size: number, maxW: number) {
  if (textWidth(font, text, size) <= maxW) return text;
  const ell = textWidth(font, "…", size);
  let w = 0;
  let i = 0;
  for (const ch of text) {
    const a = ((font.glyphs.get(ch) ?? font.glyphs.get("?")!)[4] * size) / ATLAS_PX;
    if (w + a + ell > maxW) break;
    w += a;
    i += ch.length;
  }
  return i ? text.slice(0, i) + "…" : "";
}

/** Truncate `text` from the start ("…/kept/end") so it fits `maxW` at `size`: for paths, whose end tells them apart. */
export function fitStart(font: GlyphFont, text: string, size: number, maxW: number) {
  if (textWidth(font, text, size) <= maxW) return text;
  const room = maxW - textWidth(font, "…", size);
  const chars = [...text];
  let w = 0;
  let i = chars.length;
  while (i > 0) {
    const a = ((font.glyphs.get(chars[i - 1]) ?? font.glyphs.get("?")!)[4] * size) / ATLAS_PX;
    if (w + a > room) break;
    w += a;
    i--;
  }
  return "…" + chars.slice(i).join("");
}

/** Growable typed arrays for one colour's glyph quads (pooled: meshes get exact-size copies). */
const bufPool: Buf[] = [];
class Buf {
  pos = new Float32Array(512);
  uv = new Float32Array(512);
  n = 0;
  reserve(quads: number) {
    const need = (this.n + quads) * 8;
    if (need <= this.pos.length) return;
    let cap = this.pos.length;
    while (cap < need) cap *= 2;
    const pos = new Float32Array(cap);
    pos.set(this.pos);
    this.pos = pos;
    const uv = new Float32Array(cap);
    uv.set(this.uv);
    this.uv = uv;
  }
}

/** Accumulates glyph quads per colour, then builds one mesh per colour. */
export class LabelBatch {
  private bufs = new Map<number, Buf>();

  constructor(private font: GlyphFont) {}

  /** Place `text` with its top-left at (x, y); `size` is the em size in world units. */
  add(text: string, x: number, y: number, size: number, color: number, align: "left" | "center" = "left") {
    const f = this.font;
    const s = size / ATLAS_PX;
    let b = this.bufs.get(color);
    if (!b) this.bufs.set(color, (b = bufPool.pop() ?? new Buf()));
    b.reserve(text.length);
    let pen = align === "center" ? x - textWidth(f, text, size) / 2 : x;
    const top = y - PAD * s;
    const h = f.cellH * s;
    const fallback = f.glyphs.get("?")!;
    for (const ch of text) {
      const gl = f.glyphs.get(ch) ?? fallback;
      if (ch !== " ") {
        const x0 = pen - PAD * s;
        const x1 = x0 + gl[5] * s;
        const o = b.n * 8;
        const p = b.pos;
        p[o] = x0;
        p[o + 1] = top;
        p[o + 2] = x1;
        p[o + 3] = top;
        p[o + 4] = x1;
        p[o + 5] = top + h;
        p[o + 6] = x0;
        p[o + 7] = top + h;
        const u = b.uv;
        u[o] = gl[0];
        u[o + 1] = gl[1];
        u[o + 2] = gl[2];
        u[o + 3] = gl[1];
        u[o + 4] = gl[2];
        u[o + 5] = gl[3];
        u[o + 6] = gl[0];
        u[o + 7] = gl[3];
        b.n++;
      }
      pen += gl[4] * s;
    }
  }

  /** Replace `into`'s children with the built meshes. */
  build(into: Container) {
    for (const ch of into.removeChildren()) dispose(ch);
    for (const [color, b] of this.bufs) {
      if (b.n) {
        const geometry = new MeshGeometry({ positions: b.pos.slice(0, b.n * 8), uvs: b.uv.slice(0, b.n * 8), indices: indicesFor(b.n) });
        const mesh = new Mesh({ geometry, texture: this.font.texture });
        mesh.tint = color;
        into.addChild(mesh);
      }
      b.n = 0;
      bufPool.push(b);
    }
    this.bufs.clear();
  }

  /** Drop what was added without building it (an abandoned resumable build). */
  discard() {
    for (const b of this.bufs.values()) {
      b.n = 0;
      bufPool.push(b);
    }
    this.bufs.clear();
  }
}

/** Em size at which a font's advance equals `advance` world units (monospace grids). */
export function sizeForAdvance(font: GlyphFont, advance: number) {
  return (advance * ATLAS_PX) / font.glyphs.get("M")![4];
}

export const sansFont = () => glyphFont("sans", (px) => `600 ${px}px Inter, "IBM Plex Sans", system-ui, sans-serif`);
export const monoFont = () => glyphFont("mono", (px) => `500 ${px}px "IBM Plex Mono", ui-monospace, Menlo, Consolas, monospace`);
