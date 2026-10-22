import { Container } from "pixi.js";
import type { WindowHeader } from "../contracts/model";
import { HEADER_WU, LINE_WU } from "../contracts/wire";
import { RectBatch } from "./rects";
import type { Theme } from "./theme";

/** The lines a comment is (or will be) anchored to. Editable ranges show drag handles. */
export interface LineRange {
  w: WindowHeader;
  start: number;
  end: number;
  editable: boolean;
  /** Band colour (default: the theme accent); a peeked link's colour from the side panel. */
  color?: number;
}

/**
 * The commented lines on the canvas: a band over the range, a bar in the gutter, and (while
 * composing) a handle on its top and bottom edge that drags the range a line at a time. A few
 * quads, rebuilt whenever the range or the tier's stroke scale changes.
 */
export class CommentRange {
  readonly root = new Container();
  range?: LineRange;
  private handles: Array<{ edge: "start" | "end"; x: number; y: number; w: number; h: number }> = [];
  builtFor = 0;

  set(r: LineRange | undefined) {
    this.range = r && { ...r, start: Math.min(r.start, r.end), end: Math.max(r.start, r.end) };
  }

  build(theme: Theme, wuPerPx: number) {
    for (const ch of this.root.removeChildren()) ch.destroy({ children: true });
    this.handles = [];
    this.builtFor = wuPerPx;
    const r = this.range;
    if (!r) return;
    const { w } = r;
    const y0 = w.rect.y + HEADER_WU + (r.start - w.lines.start) * LINE_WU;
    const h = (r.end - r.start + 1) * LINE_WU;
    const rb = new RectBatch(8);
    const color = r.color ?? theme.accent;
    rb.add(w.rect.x, y0, w.rect.w, h, color, r.color === undefined ? 0.13 : 0.22);
    rb.add(w.rect.x, y0, Math.max(4, 3 * wuPerPx), h, color, 0.95);
    if (r.editable) {
      // Handles: ~28×8 px pills centred on the top and bottom edges of the band, in the gutter.
      const hw = 28 * wuPerPx;
      const hh = 8 * wuPerPx;
      const x = w.rect.x + 4 * wuPerPx;
      for (const [edge, y] of [["start", y0], ["end", y0 + h]] as const) {
        rb.add(x, y - hh / 2, hw, hh, theme.accent, 1);
        rb.add(x + hw * 0.3, y - hh * 0.1, hw * 0.4, hh * 0.2, theme.bg, 0.9);
        // A generous hit area: the visible pill plus a few pixels around it.
        this.handles.push({ edge, x: x - 4 * wuPerPx, y: y - hh / 2 - 5 * wuPerPx, w: hw + 8 * wuPerPx, h: hh + 10 * wuPerPx });
      }
    }
    rb.build(this.root);
  }

  /** The handle under world point (x, y), if any. */
  handleAt(x: number, y: number): "start" | "end" | undefined {
    return this.handles.find((h) => x >= h.x && x <= h.x + h.w && y >= h.y && y <= h.y + h.h)?.edge;
  }
}
