import { Container } from "pixi.js";
import type { BlobText } from "../contracts/http";
import type { WindowDiff, WindowHeader } from "../contracts/model";
import { COL_WU, HEADER_WU, LINE_WU } from "../contracts/wire";
import { GUTTER_WU, drawDiff } from "../canvas/chunks";
import { LabelBatch, monoFont, sansFont, sizeForAdvance, textWidth } from "../canvas/labels";
import { BL, BR, RectBatch } from "../canvas/rects";
import { mix, type Theme } from "../canvas/theme";

/**
 * L3 code text for one window, drawn on the GPU from the monospace glyph atlas: an opaque body
 * (hiding the silhouette underneath), line numbers and token-coloured runs. Building is a
 * typed-array fill, so windows can stream in while the camera moves. Selection and editing belong
 * to the activated window (CodeMirror, F3), not to these meshes.
 */
/** A clickable "load more lines" pill at a window's top or bottom fade, or "collapse" (back to `expandedFrom`). */
export interface ExpandHit {
  x: number;
  y: number;
  w: number;
  h: number;
  window: WindowHeader;
  req: { before?: number | "file"; after?: number | "file"; collapse?: true };
}

const MORE = 20;

export function buildCodeText(w: WindowHeader, text: BlobText, t: Theme, diff?: WindowDiff, hits?: ExpandHit[], ownerSlot?: (owner: string) => number): Container {
  const c = new Container();
  const r = w.rect;
  const bg = new RectBatch(16);
  bg.round(r.x + 2, r.y + HEADER_WU, r.w - 4, r.h - HEADER_WU - 2, 4, t.body, 1, BL | BR);
  // Range views: added-line tints and removed-line markers under the text.
  drawDiff(bg, w, diff, 1, ownerSlot);
  const bgc = new Container();
  bg.build(bgc);
  c.addChild(bgc);

  const font = monoFont();
  const size = sizeForAdvance(font, COL_WU);
  const batch = new LabelBatch(font);
  const x0 = r.x + GUTTER_WU;
  const maxCols = Math.floor((r.w - GUTTER_WU - 12) / COL_WU);
  const first = Math.max(w.lines.start, text.from);
  const last = Math.min(w.lines.end, text.to);
  for (let n = first; n <= last; n++) {
    const i = n - text.from;
    const y = r.y + HEADER_WU + (n - w.lines.start) * LINE_WU + 1.5;
    const num = String(n);
    batch.add(num, x0 - 10 - num.length * COL_WU * 0.8, y + 1.5, size * 0.8, t.dim);
    const line = text.lines[i];
    const tok = text.tokens[i] ?? [];
    let at = 0;
    const run = (from: number, to: number, color: number) => {
      const a = Math.min(from, maxCols);
      const b = Math.min(to, maxCols);
      if (b > a) batch.add(line.slice(a, b), x0 + a * COL_WU, y, size, color);
    };
    for (let j = 0; j < tok.length; j += 3) {
      const [s, len, cls] = [tok[j], tok[j + 1], tok[j + 2]];
      if (s > at) run(at, s, t.ink);
      run(s, s + len, t.token[cls] ?? t.ink);
      at = s + len;
    }
    if (at < line.length) run(at, line.length, t.ink);
  }
  // Fades where the file continues beyond the window, with pills that widen it in place; an
  // expanded window also gets a "collapse" pill at its bottom, after the others.
  const above = w.lines.start - 1;
  const below = w.lines.fileLines - w.lines.end;
  const collapsible = !!w.expandedFrom;
  const fade = new RectBatch(16);
  const pills = new LabelBatch(sansFont());
  const edge = (top: boolean, n: number, collapse = false) => {
    const y = top ? r.y + HEADER_WU : r.y + r.h - 3 * LINE_WU;
    for (let k = 0; k < 3; k++) {
      const row = top ? k : 2 - k;
      const last = !top && k === 2;
      fade.round(r.x + 2, y + k * LINE_WU, r.w - 4, last ? LINE_WU - 2 : LINE_WU, last ? 4 : 0, t.body, [0.75, 0.45, 0.2][row], BL | BR);
    }
    const size = 13;
    const pillH = 20;
    const py = top ? y + 4 : y + 3 * LINE_WU - pillH - 4;
    const rim = mix(t.header, t.line, 0.35);
    const pill = (x: number, w: number) => {
      fade.round(x, py, w, pillH, pillH / 2, rim);
      fade.round(x + 1, py + 1, w - 2, pillH - 2, pillH / 2 - 1, t.header);
    };
    const req = (v: number | "file") => (top ? { before: v } : { after: v });
    const label = `${top ? "↑" : "↓"} ${n} lines ${top ? "above" : "below"}`;
    const lw = n > 0 ? textWidth(sansFont(), label, size) + 20 : 0;
    const moreW = n > 0 ? textWidth(sansFont(), `+${MORE}`, size) + 16 : 0;
    const allW = n > 0 ? textWidth(sansFont(), "all", size) + 16 : 0;
    const colW = collapse ? textWidth(sansFont(), "collapse", size) + 16 : 0;
    const total = n > 0 ? lw + moreW + allW + 12 + (collapse ? colW + 6 : 0) : colW;
    let px = r.x + r.w / 2 - total / 2;
    if (n > 0) {
      pill(px, lw);
      pills.add(label, px + 10, py + 3, size, t.ink);
      fade.round(px + lw + 6, py, moreW, pillH, pillH / 2, t.accent);
      pills.add(`+${MORE}`, px + lw + 14, py + 3, size, t.bg);
      pill(px + lw + moreW + 12, allW);
      pills.add("all", px + lw + moreW + 20, py + 3, size, t.ink);
      hits?.push({ x: px, y: py, w: lw + moreW + 6, h: pillH, window: w, req: req(MORE) });
      hits?.push({ x: px + lw + moreW + 12, y: py, w: allW, h: pillH, window: w, req: req("file") });
      px += lw + moreW + allW + 18;
    }
    if (collapse) {
      pill(px, colW);
      pills.add("collapse", px + 8, py + 3, size, t.ink);
      hits?.push({ x: px, y: py, w: colW, h: pillH, window: w, req: { collapse: true } });
    }
  };
  if (above > 0) edge(true, above);
  if (below > 0 || collapsible) edge(false, below, collapsible);
  const glyphs = new Container();
  batch.build(glyphs);
  c.addChild(glyphs);
  if (above > 0 || below > 0 || collapsible) {
    const f = new Container();
    fade.build(f);
    const p = new Container();
    pills.build(p);
    c.addChild(f, p);
  }
  return c;
}
